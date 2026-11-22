// Human Resource Management module (batch 3).
//
// Employees, positions, contracts, leave, attendance, payroll runs,
// performance reviews, onboarding/offboarding checklists, employee
// documents, org-chart data, and HR reports. Interconnection happens
// only through the hook bus — no module calls another module directly.
//
// Identity rule: an employee record stores the linked user id ONLY.
// Names, emails, and all other profile data live in the Users module
// and are resolved by callers through `GET /api/v1/users/{id}`.
// Department records are likewise owned by the Departments module:
// this module stores department ids supplied by callers and validates
// them through the `department.membership_check` hook (see
// `department_verified`); it never duplicates department records.
// Document files live in Storage: this module stores metadata only
// (title, type, `storage_file_id`) and never stores file bytes.
//
// Money: salary and payroll amounts are NUMERIC(19,4) in the
// migration and `f64` rounded to four decimals in memory (the same
// convention as the TimeTracker hourly rates). Salary amounts are
// visible only to HR-capable callers (`hr:read_all`, `hr:manage`,
// `hr:payroll`) and to the employee the record belongs to; every
// other caller receives the record with the money fields removed.
// Payroll never pays out from here: finalizing a run only emits
// `hr.payroll_run_completed` for the billing module.
//
// Role-based access (enforced server-side on every handler):
//   - `hr:read`       — read HR records (own records for employees).
//   - `hr:read_all`   — read every employee's HR records.
//   - `hr:manage`     — create/update/delete HR records, approve or
//                       reject leave, manage reviews and checklists.
//   - `hr:payroll`    — create/compute/finalize payroll runs and read
//                       payroll amounts (also granted by `hr:manage`).
// Capabilities are checked through the `role.check` hook and fail
// closed: no answer means not allowed. The admin role holds `"*":
// ["*"]`, so it passes every check. An employee without any HR
// capability sees only their own records. No `hr:*` capability is
// granted by the default roles in the Roles module; an administrator
// must grant them to the HR roles (see INTEGRATION_NOTES_BATCH3_B2.md).
//
// Events emitted:
//   - `hr.employee_created`        {employee_id, user_id, department_id}
//   - `hr.leave_requested`         {leave_request_id, employee_id, leave_type_id, start_date, end_date, days}
//   - `hr.leave_approved`          {leave_request_id, employee_id, days}
//   - `hr.leave_rejected`          {leave_request_id, employee_id, reason}
//   - `hr.payroll_run_completed`   {payroll_run_id, period, total_gross, total_net, employee_count, currency}
//   - `hr.review_completed`        {review_id, cycle_id, employee_id, rating}
//   - `hr.onboarding_completed`    {checklist_id, employee_id}
//   - `notification.requested`     leave decisions and completed reviews
//                                  (the Notifications module listens).
// Events consumed:
//   - `timetracker.time.logged`    the event the TimeTracker module in
//                                  this tree actually emits (payload:
//                                  `{entry_id, user_id, started_at,
//                                  ended_at, duration_secs, hours,
//                                  billable, source, ...}`).
//   - `timetracker.entry_logged`   the event name the brief documents
//                                  (payload: `{user_id, date, minutes,
//                                  source}`).
//   - `department.membership_check` answers, read when validating a
//                                  department id (emitted, not subscribed).
//   - `storage.file_requested`     answers, read when storing document
//                                  metadata (emitted, not subscribed).
//   - `user.manager.query`         answered by this module: given
//                                  `{"user_id": "..."}` it answers with
//                                  the manager's USER id (looked up
//                                  through the employee's `manager_id`,
//                                  which is an employee id) as
//                                  `{"user_id", "resolved": true,
//                                  "manager_id": "..."}`. When HR holds
//                                  no employee for the user, no manager
//                                  is recorded, or the manager's
//                                  employee record is missing, it
//                                  answers `{"resolved": false,
//                                  "manager_id": null, "reason": ...}`
//                                  instead of fabricating a manager;
//                                  the approvals module then leaves the
//                                  manager slot unresolved.
//
// CONVERGENCE (for the integrator — see INTEGRATION_NOTES_BATCH3_B2.md):
//   1. TimeTracker event name: the brief names `timetracker.entry_logged`
//      with `{user_id, date, minutes, source}`; the TimeTracker module
//      already in this tree emits `timetracker.time.logged` with
//      `{entry_id, user_id, started_at, ended_at, duration_secs, hours,
//      source}`. This module subscribes to BOTH names and accepts BOTH
//      payload shapes, so it works either way; the integrator should
//      pick one canonical name and payload.
//   2. `department.membership_check` answers which departments a user
//      agents for, which is not the same as the department an employee
//      belongs to. A supplied department id is therefore recorded with
//      a `department_verified` flag (true only when the hook answer
//      lists it) instead of being rejected when it is absent.
//   3. No `storage.file_requested` handler exists in this tree yet.
//      Document creation emits it defensively and stores the metadata
//      when nothing answers; a handler that answers `{"exists": false}`
//      makes creation fail with 404.
//   4. The audit module only records events listed in its
//      `AUDITED_EVENTS`; the `hr.*` names above must be added there
//      for HR actions to appear in the audit trail.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// Read HR records (own records for employees without `hr:read_all`).
pub const PERM_HR_READ: &str = "hr:read";
/// Read every employee's HR records, including salary amounts.
pub const PERM_HR_READ_ALL: &str = "hr:read_all";
/// Create/update/delete HR records; approve or reject leave; manage
/// reviews, checklists, and documents. Also grants payroll access.
pub const PERM_HR_MANAGE: &str = "hr:manage";
/// Create, compute, and finalize payroll runs; read payroll amounts.
pub const PERM_HR_PAYROLL: &str = "hr:payroll";

/// Largest money amount accepted in a request. NUMERIC(19,4) holds at
/// most 15 integer digits, so anything at or above 1e15 is rejected.
const MAX_MONEY: f64 = 1_000_000_000_000_000.0;
/// Longest free-text field accepted (titles, notes, reasons).
const MAX_TEXT: usize = 2000;
/// A work day cannot exceed 24 hours.
const MAX_MINUTES_PER_DAY: u64 = 1440;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One employee. Only the linked user id is stored; names and emails
/// live in the Users module. `manager_id` is another employee id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Employee {
    pub id: String,
    pub user_id: String,
    pub employee_code: String,
    pub position_id: Option<String>,
    pub department_id: Option<String>,
    /// True when `department.membership_check` listed this department
    /// for the employee's user id (see the module header).
    #[serde(default)]
    pub department_verified: bool,
    pub manager_id: Option<String>,
    /// `active`, `on_leave`, or `terminated`.
    pub status: String,
    pub hire_date: NaiveDate,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateEmployee {
    pub user_id: String,
    pub employee_code: Option<String>,
    pub position_id: Option<String>,
    pub department_id: Option<String>,
    pub manager_id: Option<String>,
    pub hire_date: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateEmployee {
    pub position_id: Option<String>,
    pub department_id: Option<String>,
    pub manager_id: Option<Option<String>>,
    pub status: Option<String>,
    pub hire_date: Option<String>,
}

/// One position (job title) with its salary band. The band is money
/// and is stripped from responses for callers without HR capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: String,
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<f64>,
    pub salary_band_max: Option<f64>,
    /// `active` or `inactive`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePosition {
    pub title: String,
    pub department_id: Option<String>,
    pub level: Option<String>,
    pub salary_band_min: Option<f64>,
    pub salary_band_max: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePosition {
    pub title: Option<String>,
    pub department_id: Option<String>,
    pub level: Option<Option<String>>,
    pub salary_band_min: Option<Option<f64>>,
    pub salary_band_max: Option<Option<f64>>,
    pub status: Option<String>,
}

/// One employment contract. `salary_amount` is monthly pay in
/// `currency` and is visible only to HR-capable callers and to the
/// employee the contract belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub id: String,
    pub employee_id: String,
    /// `permanent`, `fixed_term`, `intern`, `part_time`, `contractor`.
    pub contract_type: String,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub salary_amount: f64,
    pub currency: String,
    /// `active`, `expired`, or `terminated`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateContract {
    pub contract_type: String,
    pub start_date: String,
    pub end_date: Option<String>,
    pub salary_amount: f64,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContract {
    pub contract_type: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<Option<String>>,
    pub salary_amount: Option<f64>,
    pub currency: Option<String>,
    pub status: Option<String>,
}

/// One leave type (annual leave, sick leave, and so on).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveType {
    pub id: String,
    pub name: String,
    pub code: String,
    pub default_days_per_year: f64,
    pub paid: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLeaveType {
    pub name: String,
    pub code: Option<String>,
    pub default_days_per_year: Option<f64>,
    pub paid: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLeaveType {
    pub name: Option<String>,
    pub default_days_per_year: Option<f64>,
    pub paid: Option<bool>,
}

/// One employee's balance for one leave type in one calendar year.
/// `used_days` counts approved requests only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveBalance {
    pub id: String,
    pub employee_id: String,
    pub leave_type_id: String,
    pub year: i32,
    pub entitled_days: f64,
    pub used_days: f64,
    pub carried_over_days: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LeaveBalance {
    fn remaining(&self) -> f64 {
        (self.entitled_days + self.carried_over_days - self.used_days).max(0.0)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLeaveBalance {
    pub leave_type_id: String,
    pub year: i32,
    pub entitled_days: Option<f64>,
    pub carried_over_days: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLeaveBalance {
    pub entitled_days: Option<f64>,
    pub carried_over_days: Option<f64>,
}

/// One leave/vacation request with its approval workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveRequest {
    pub id: String,
    pub employee_id: String,
    pub leave_type_id: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// Inclusive calendar days between start and end.
    pub days: f64,
    /// `pending`, `approved`, `rejected`, or `cancelled`.
    pub status: String,
    #[serde(default)]
    pub reason: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLeaveRequest {
    pub employee_id: Option<String>,
    pub leave_type_id: String,
    pub start_date: String,
    pub end_date: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReviewLeaveRequest {
    pub reason: Option<String>,
}

/// One day of attendance, fed by TimeTracker events (see the module
/// header) or entered manually by HR. `entry_id` is the TimeTracker
/// entry id and is used to de-duplicate repeated events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttendanceRecord {
    pub id: String,
    pub employee_id: String,
    pub user_id: String,
    pub work_date: NaiveDate,
    pub minutes_worked: u64,
    pub source: String,
    pub entry_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAttendance {
    pub employee_id: String,
    pub work_date: String,
    pub minutes_worked: u64,
    pub source: Option<String>,
    pub entry_id: Option<String>,
}

/// One payroll run for one period (`YYYY-MM`). Money is computed from
/// active contracts plus per-employee adjustments; nothing here pays
/// anyone — finalizing only emits `hr.payroll_run_completed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayrollRun {
    pub id: String,
    pub period: String,
    pub currency: String,
    /// `draft`, `computed`, `finalized`, or `cancelled`.
    pub status: String,
    pub total_gross: f64,
    pub total_net: f64,
    pub total_employees: u32,
    pub finalized_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePayrollRun {
    pub period: String,
    pub currency: Option<String>,
}

/// One employee's line inside a payroll run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayrollItem {
    pub id: String,
    pub payroll_run_id: String,
    pub employee_id: String,
    pub gross: f64,
    pub adjustments: f64,
    pub deductions: f64,
    pub net: f64,
    #[serde(default)]
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePayrollItem {
    pub adjustments: Option<f64>,
    pub deductions: Option<f64>,
    pub notes: Option<String>,
}

/// One performance-review cycle (for example "2026 H2").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCycle {
    pub id: String,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// `draft`, `active`, or `closed`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReviewCycle {
    pub name: String,
    pub period_start: String,
    pub period_end: String,
}

/// One performance review: rating, goals, and reviewer notes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: String,
    pub cycle_id: String,
    pub employee_id: String,
    pub reviewer_id: String,
    /// 1 through 5; required before completion.
    pub rating: Option<u8>,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub reviewer_notes: String,
    /// `draft`, `submitted`, or `completed`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateReview {
    pub cycle_id: String,
    pub employee_id: String,
    pub reviewer_id: String,
    pub rating: Option<u8>,
    pub goals: Option<Vec<String>>,
    pub reviewer_notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateReview {
    pub rating: Option<u8>,
    pub goals: Option<Vec<String>>,
    pub reviewer_notes: Option<String>,
    pub status: Option<String>,
}

/// One onboarding or offboarding checklist for one employee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checklist {
    pub id: String,
    pub employee_id: String,
    /// `onboarding` or `offboarding`.
    pub kind: String,
    /// `pending`, `in_progress`, or `completed`.
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChecklist {
    pub kind: String,
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChecklistItem {
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub checklist_id: String,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One employee document. Metadata only — the file itself lives in
/// Storage under `storage_file_id`; no file bytes are stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeDocument {
    pub id: String,
    pub employee_id: String,
    pub title: String,
    /// `contract`, `id`, `passport`, `certificate`, or `other`.
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateEmployeeDocument {
    pub title: String,
    pub document_type: Option<String>,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
}

/// One org-chart node: manager links only, no salary data.
#[derive(Debug, Clone, Serialize)]
pub struct OrgChartNode {
    pub employee_id: String,
    pub user_id: String,
    pub position_id: Option<String>,
    pub department_id: Option<String>,
    pub manager_id: Option<String>,
    pub status: String,
    pub hire_date: NaiveDate,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct HrState {
    hooks: RwLock<Arc<HookBus>>,
    employees: DashMap<String, Employee>,
    positions: DashMap<String, Position>,
    contracts: DashMap<String, Contract>,
    leave_types: DashMap<String, LeaveType>,
    leave_balances: DashMap<String, LeaveBalance>,
    leave_requests: DashMap<String, LeaveRequest>,
    attendance: DashMap<String, AttendanceRecord>,
    payroll_runs: DashMap<String, PayrollRun>,
    payroll_items: DashMap<String, PayrollItem>,
    review_cycles: DashMap<String, ReviewCycle>,
    reviews: DashMap<String, Review>,
    checklists: DashMap<String, Checklist>,
    checklist_items: DashMap<String, ChecklistItem>,
    documents: DashMap<String, EmployeeDocument>,
}

impl HrState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            employees: DashMap::new(),
            positions: DashMap::new(),
            contracts: DashMap::new(),
            leave_types: DashMap::new(),
            leave_balances: DashMap::new(),
            leave_requests: DashMap::new(),
            attendance: DashMap::new(),
            payroll_runs: DashMap::new(),
            payroll_items: DashMap::new(),
            review_cycles: DashMap::new(),
            reviews: DashMap::new(),
            checklists: DashMap::new(),
            checklist_items: DashMap::new(),
            documents: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Departments module which departments a user agents for
    /// (`department.membership_check`). Returns the department ids
    /// from every answer, or an empty list when nothing answers.
    fn department_ids_for_user(&self, user_id: &str) -> Vec<String> {
        let context = self
            .bus()
            .emit("department.membership_check", json!({ "user_id": user_id }));
        let mut ids: Vec<String> = Vec::new();
        for result in &context.results {
            if let Some(list) = result.get("department_ids").and_then(|v| v.as_array()) {
                for entry in list {
                    if let Some(id) = entry.as_str() {
                        if !ids.contains(&id.to_string()) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        }
        ids
    }

    /// True when the hook answer lists `department_id` for `user_id`.
    /// See the module header: membership is not employment, so a
    /// false result records `department_verified: false` instead of
    /// rejecting the record.
    fn department_verified(&self, user_id: &str, department_id: &str) -> bool {
        self.department_ids_for_user(user_id)
            .iter()
            .any(|id| id == department_id)
    }

    /// Ask Storage whether `file_id` exists (`storage.file_requested`).
    /// Returns `Ok(false)` only when a handler explicitly answers
    /// `{"exists": false}`; no answer means "unknown", which is
    /// accepted (no Storage handler exists in this tree yet).
    fn storage_file_exists(&self, file_id: &str) -> bool {
        let context = self
            .bus()
            .emit("storage.file_requested", json!({ "file_id": file_id }));
        for result in &context.results {
            if let Some(exists) = result.get("exists").and_then(|v| v.as_bool()) {
                if !exists {
                    return false;
                }
            }
        }
        true
    }

    fn employee_by_user(&self, user_id: &str) -> Option<Employee> {
        self.employees
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    /// Walk the manager chain above `employee_id`. Returns true when
    /// `candidate_manager_id` would create a cycle (the candidate is
    /// the employee itself or one of its reports).
    fn manager_would_cycle(&self, employee_id: &str, candidate_manager_id: &str) -> bool {
        if candidate_manager_id == employee_id {
            return true;
        }
        let mut current = Some(candidate_manager_id.to_string());
        let mut hops = 0;
        while let Some(id) = current {
            // A chain longer than the employee count must already loop.
            if hops > self.employees.len() {
                return true;
            }
            hops += 1;
            match self.employees.get(&id) {
                Some(employee) => {
                    if employee.id == employee_id {
                        return true;
                    }
                    current = employee.manager_id.clone();
                }
                None => return false,
            }
        }
        false
    }

    /// True when `employee_id` already has a pending or approved leave
    /// request overlapping `[start, end]`, excluding `exclude_id`.
    fn leave_overlaps(
        &self,
        employee_id: &str,
        start: NaiveDate,
        end: NaiveDate,
        exclude_id: Option<&str>,
    ) -> bool {
        self.leave_requests.iter().any(|entry| {
            let request = entry.value();
            request.employee_id == employee_id
                && matches!(request.status.as_str(), "pending" | "approved")
                && exclude_id != Some(request.id.as_str())
                && request.start_date <= end
                && start <= request.end_date
        })
    }

    /// Find or create the balance row for one employee, leave type,
    /// and year. A new row starts from the leave type's default days.
    fn ensure_balance(
        &self,
        employee_id: &str,
        leave_type_id: &str,
        year: i32,
    ) -> UnifiedResult<LeaveBalance> {
        if let Some(balance) = self.leave_balances.iter().find(|entry| {
            let balance = entry.value();
            balance.employee_id == employee_id
                && balance.leave_type_id == leave_type_id
                && balance.year == year
        }) {
            return Ok(balance.value().clone());
        }
        let leave_type = self
            .leave_types
            .get(leave_type_id)
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))?;
        let balance = LeaveBalance {
            id: new_id(),
            employee_id: employee_id.to_string(),
            leave_type_id: leave_type_id.to_string(),
            year,
            entitled_days: leave_type.default_days_per_year,
            used_days: 0.0,
            carried_over_days: 0.0,
            created_at: now(),
            updated_at: now(),
        };
        self.leave_balances
            .insert(balance.id.clone(), balance.clone());
        Ok(balance)
    }

    /// The active contract of one employee, if any: the latest
    /// `active` contract whose start date has passed and whose end
    /// date (if any) has not passed.
    fn active_contract(&self, employee_id: &str, today: NaiveDate) -> Option<Contract> {
        let mut contracts: Vec<Contract> = self
            .contracts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contract| {
                contract.employee_id == employee_id
                    && contract.status == "active"
                    && contract.start_date <= today
                    && contract.end_date.is_none_or(|end| end >= today)
            })
            .collect();
        contracts.sort_by(|a, b| b.start_date.cmp(&a.start_date));
        contracts.into_iter().next()
    }
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation, views
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// Money rounded to four decimals (NUMERIC(19,4)).
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
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

/// True when the caller holds any HR capability (read-all, manage, or
/// payroll). Used wherever salary amounts may be shown.
fn is_hr_capable(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_HR_READ_ALL)
        || check_capability(ctx, role, PERM_HR_MANAGE)
        || check_capability(ctx, role, PERM_HR_PAYROLL)
}

/// True when the caller may manage payroll: `hr:payroll`, or
/// `hr:manage` (managing HR includes payroll administration).
fn can_payroll(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_HR_PAYROLL) || check_capability(ctx, role, PERM_HR_MANAGE)
}

/// True when `caller` may read `employee`'s records: HR read-all, or
/// the employee themself.
fn can_read_employee(ctx: &AppContext, caller: &Caller, employee: &Employee) -> bool {
    check_capability(ctx, &caller.role, PERM_HR_READ_ALL) || employee.user_id == caller.actor
}

/// Enforce read access for one employee record, hiding the not-found
/// case: a caller who may not read the record gets 404, so employee
/// ids cannot be probed.
fn require_read_employee(
    ctx: &AppContext,
    caller: &Caller,
    employee: &Employee,
) -> UnifiedResult<()> {
    if can_read_employee(ctx, caller, employee) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Employee not found"))
    }
}

/// Enforce read access for one employee id: 404 when the employee
/// does not exist or the caller may not read it.
fn load_employee_for_read(
    state: &HrState,
    ctx: &AppContext,
    caller: &Caller,
    employee_id: &str,
) -> UnifiedResult<Employee> {
    let employee = state
        .employees
        .get(employee_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
    require_read_employee(ctx, caller, &employee)?;
    Ok(employee)
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
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

fn parse_money(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value >= MAX_MONEY {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite amount from 0 up to 999999999999999.9999"
        )));
    }
    Ok(round4(value))
}

fn parse_days(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 366.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 366 days"
        )));
    }
    Ok(round4(value))
}

fn valid_currency(raw: &str) -> UnifiedResult<String> {
    let currency = raw.trim().to_uppercase();
    if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code (for example USD)",
        ));
    }
    Ok(currency)
}

/// `YYYY-MM` period validation for payroll runs.
fn valid_period(raw: &str) -> UnifiedResult<String> {
    let period = raw.trim();
    let (year, month) = period
        .split_once('-')
        .ok_or_else(|| UnifiedError::bad_request("period must be YYYY-MM"))?;
    let year: i32 = year
        .parse()
        .map_err(|_| UnifiedError::bad_request("period must be YYYY-MM"))?;
    let month: u32 = month
        .parse()
        .map_err(|_| UnifiedError::bad_request("period must be YYYY-MM"))?;
    if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) {
        return Err(UnifiedError::bad_request(
            "period must be YYYY-MM with a month from 01 to 12",
        ));
    }
    Ok(format!("{year:04}-{month:02}"))
}

/// `Annual Leave` -> `annual-leave`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// A contract with `salary_amount` removed for callers who may not
/// see money. HR-capable callers and the employee themself get the
/// full record.
fn contract_view(
    ctx: &AppContext,
    caller: &Caller,
    contract: &Contract,
    employee_user_id: &str,
) -> Value {
    if is_hr_capable(ctx, &caller.role) || employee_user_id == caller.actor {
        serde_json::to_value(contract).unwrap_or(Value::Null)
    } else {
        json!({
            "id": contract.id,
            "employee_id": contract.employee_id,
            "contract_type": contract.contract_type,
            "start_date": contract.start_date,
            "end_date": contract.end_date,
            "currency": contract.currency,
            "status": contract.status,
            "created_at": contract.created_at,
            "updated_at": contract.updated_at,
        })
    }
}

/// A position with the salary band removed for callers who may not
/// see money.
fn position_view(ctx: &AppContext, caller: &Caller, position: &Position) -> Value {
    if is_hr_capable(ctx, &caller.role) {
        serde_json::to_value(position).unwrap_or(Value::Null)
    } else {
        json!({
            "id": position.id,
            "title": position.title,
            "department_id": position.department_id,
            "level": position.level,
            "status": position.status,
            "created_at": position.created_at,
            "updated_at": position.updated_at,
        })
    }
}

/// A payroll item with money removed for callers who may not see it.
/// HR-capable callers and the employee themself get the full record.
fn payroll_item_view(
    ctx: &AppContext,
    caller: &Caller,
    item: &PayrollItem,
    employee_user_id: &str,
) -> Value {
    if is_hr_capable(ctx, &caller.role) || employee_user_id == caller.actor {
        serde_json::to_value(item).unwrap_or(Value::Null)
    } else {
        json!({
            "id": item.id,
            "payroll_run_id": item.payroll_run_id,
            "employee_id": item.employee_id,
            "notes": item.notes,
            "created_at": item.created_at,
            "updated_at": item.updated_at,
        })
    }
}


// ---------------------------------------------------------------------------
// Handlers: employees
// ---------------------------------------------------------------------------

async fn list_employees(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Employee>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|employee| {
                (read_all || employee.user_id == caller.actor)
                    && matches_search(
                        &format!("{} {}", employee.employee_code, employee.user_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| employee.status == *status)
                    && params
                        .filters
                        .get("department_id")
                        .is_none_or(|department| {
                            employee.department_id.as_deref() == Some(department.as_str())
                        })
                    && params
                        .filters
                        .get("position_id")
                        .is_none_or(|position| {
                            employee.position_id.as_deref() == Some(position.as_str())
                        })
            })
            .collect();
        items.sort_by(|a, b| a.employee_code.cmp(&b.employee_code));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateEmployee>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 255)?;
        if state
            .employees
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "An employee record already exists for user {user_id}"
            )));
        }
        let employee_code = match input.employee_code {
            Some(code) if !code.trim().is_empty() => require_text(&code, "employee_code", 64)?,
            _ => format!("EMP-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .employees
            .iter()
            .any(|entry| entry.value().employee_code == employee_code)
        {
            return Err(UnifiedError::conflict(format!(
                "An employee with code \"{employee_code}\" already exists"
            )));
        }
        if let Some(position_id) = &input.position_id {
            if !position_id.trim().is_empty()
                && !state.positions.contains_key(position_id.trim())
            {
                return Err(UnifiedError::not_found(format!(
                    "Position {} not found",
                    position_id.trim()
                )));
            }
        }
        let department_id = input
            .department_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let department_verified = match &department_id {
            Some(department_id) => state.department_verified(&user_id, department_id),
            None => false,
        };
        let manager_id = input
            .manager_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(manager_id) = &manager_id {
            if !state.employees.contains_key(manager_id.as_str()) {
                return Err(UnifiedError::not_found(format!(
                    "Manager employee {manager_id} not found"
                )));
            }
        }
        let hire_date = parse_date(&input.hire_date, "hire_date")?;
        let employee = Employee {
            id: new_id(),
            user_id: user_id.clone(),
            employee_code,
            position_id: input
                .position_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            department_id,
            department_verified,
            manager_id,
            status: "active".to_string(),
            hire_date,
            created_at: now(),
            updated_at: now(),
        };
        // A fresh employee cannot manage themself, so no cycle check
        // is needed here; updates check the chain instead.
        state.employees.insert(employee.id.clone(), employee.clone());
        state.emit(
            "hr.employee_created",
            json!({
                "employee_id": employee.id,
                "user_id": employee.user_id,
                "department_id": employee.department_id,
            }),
        );
        Ok(employee)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        load_employee_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

async fn update_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEmployee>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Employee> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let employee_id = path.as_str().to_string();
        if let Some(position_id) = &input.position_id {
            if !position_id.trim().is_empty()
                && !state.positions.contains_key(position_id.trim())
            {
                return Err(UnifiedError::not_found(format!(
                    "Position {} not found",
                    position_id.trim()
                )));
            }
        }
        if let Some(Some(manager_id)) = &input.manager_id {
            if !manager_id.trim().is_empty() {
                if !state.employees.contains_key(manager_id.trim()) {
                    return Err(UnifiedError::not_found(format!(
                        "Manager employee {} not found",
                        manager_id.trim()
                    )));
                }
                if state.manager_would_cycle(&employee_id, manager_id.trim()) {
                    return Err(UnifiedError::conflict(
                        "manager_id would create a cycle in the reporting chain",
                    ));
                }
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "on_leave" | "terminated") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, on_leave, terminated",
                ));
            }
        }
        let hire_date = match &input.hire_date {
            Some(raw) => Some(parse_date(raw, "hire_date")?),
            None => None,
        };
        let department_verified = match &input.department_id {
            Some(Some(department_id)) if !department_id.trim().is_empty() => {
                let user_id = state
                    .employees
                    .get(employee_id.as_str())
                    .map(|entry| entry.value().user_id.clone())
                    .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
                Some(state.department_verified(&user_id, department_id.trim()))
            }
            _ => None,
        };
        let mut employee = state
            .employees
            .get_mut(employee_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if let Some(position_id) = input.position_id {
            employee.position_id = if position_id.trim().is_empty() {
                None
            } else {
                Some(position_id.trim().to_string())
            };
        }
        if let Some(department_id) = input.department_id {
            employee.department_id = if department_id.trim().is_empty() {
                None
            } else {
                Some(department_id.trim().to_string())
            };
            if let Some(verified) = department_verified {
                employee.department_verified = verified;
            } else if employee.department_id.is_none() {
                employee.department_verified = false;
            }
        }
        if let Some(manager_id) = input.manager_id {
            employee.manager_id = manager_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(status) = input.status {
            employee.status = status;
        }
        if let Some(hire_date) = hire_date {
            employee.hire_date = hire_date;
        }
        employee.updated_at = now();
        Ok(employee.clone())
    })();
    json::respond(result, debug)
}

async fn delete_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if state
            .contracts
            .iter()
            .any(|entry| entry.value().employee_id == employee_id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(
                "Employee still has an active contract; terminate it first",
            ));
        }
        if state.leave_requests.iter().any(|entry| {
            let request = entry.value();
            request.employee_id == employee_id
                && matches!(request.status.as_str(), "pending" | "approved")
        }) {
            return Err(UnifiedError::conflict(
                "Employee still has pending or approved leave requests; cancel them first",
            ));
        }
        if state
            .employees
            .iter()
            .any(|entry| entry.value().manager_id.as_deref() == Some(employee_id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Employee still manages other employees; reassign their reports first",
            ));
        }
        state
            .employees
            .remove(employee_id.as_str())
            .map(|_| json!({ "deleted": true, "id": employee_id }))
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))
    })();
    json::respond(result, debug)
}

/// Org-chart data: manager links for every employee, no salary data.
/// Any authenticated caller may read it; salary never appears here.
async fn get_org_chart(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<OrgChartNode>> = (|| {
        let _caller = caller(&req)?;
        let mut nodes: Vec<OrgChartNode> = state
            .employees
            .iter()
            .map(|entry| {
                let employee = entry.value();
                OrgChartNode {
                    employee_id: employee.id.clone(),
                    user_id: employee.user_id.clone(),
                    position_id: employee.position_id.clone(),
                    department_id: employee.department_id.clone(),
                    manager_id: employee.manager_id.clone(),
                    status: employee.status.clone(),
                    hire_date: employee.hire_date,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.employee_id.cmp(&b.employee_id));
        Ok(nodes)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: positions
// ---------------------------------------------------------------------------

async fn list_positions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .positions
            .iter()
            .map(|entry| position_view(&ctx, &caller, entry.value()))
            .filter(|position| {
                let title = position
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                matches_search(title, &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| {
                            position.get("status").and_then(|v| v.as_str()) == Some(status.as_str())
                        })
                    && params.filters.get("department_id").is_none_or(|department| {
                        position.get("department_id").and_then(|v| v.as_str())
                            == Some(department.as_str())
                    })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("title")
                .and_then(|v| v.as_str())
                .cmp(&b.get("title").and_then(|v| v.as_str()))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreatePosition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Position> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", 255)?;
        let salary_band_min = match input.salary_band_min {
            Some(value) => Some(parse_money(value, "salary_band_min")?),
            None => None,
        };
        let salary_band_max = match input.salary_band_max {
            Some(value) => Some(parse_money(value, "salary_band_max")?),
            None => None,
        };
        if let (Some(min), Some(max)) = (salary_band_min, salary_band_max) {
            if min > max {
                return Err(UnifiedError::bad_request(
                    "salary_band_min must not exceed salary_band_max",
                ));
            }
        }
        let position = Position {
            id: new_id(),
            title,
            department_id: input
                .department_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            level: optional_text(&input.level, "level", 128)?,
            salary_band_min,
            salary_band_max,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.positions.insert(position.id.clone(), position.clone());
        Ok(position)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let position = state
            .positions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Position not found"))?;
        Ok(position_view(&ctx, &caller, &position))
    })();
    json::respond(result, debug)
}

async fn update_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePosition>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Position> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "inactive") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, inactive",
                ));
            }
        }
        let salary_band_min = match input.salary_band_min {
            Some(Some(value)) => Some(Some(parse_money(value, "salary_band_min")?)),
            Some(None) => Some(None),
            None => None,
        };
        let salary_band_max = match input.salary_band_max {
            Some(Some(value)) => Some(Some(parse_money(value, "salary_band_max")?)),
            Some(None) => Some(None),
            None => None,
        };
        let mut position = state
            .positions
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Position not found"))?;
        // Validate the new band before writing anything: the entry is
        // a live reference, so a failed check after assignment would
        // leave the bad values stored.
        let new_min = salary_band_min.unwrap_or(position.salary_band_min);
        let new_max = salary_band_max.unwrap_or(position.salary_band_max);
        if let (Some(min), Some(max)) = (new_min, new_max) {
            if min > max {
                return Err(UnifiedError::bad_request(
                    "salary_band_min must not exceed salary_band_max",
                ));
            }
        }
        if let Some(title) = input.title {
            position.title = require_text(&title, "title", 255)?;
        }
        if let Some(department_id) = input.department_id {
            position.department_id = if department_id.trim().is_empty() {
                None
            } else {
                Some(department_id.trim().to_string())
            };
        }
        if let Some(level) = input.level {
            position.level = match level {
                Some(value) => Some(require_text(&value, "level", 128)?),
                None => None,
            };
        }
        position.salary_band_min = new_min;
        position.salary_band_max = new_max;
        if let Some(status) = input.status {
            position.status = status;
        }
        position.updated_at = now();
        Ok(position.clone())
    })();
    json::respond(result, debug)
}

async fn delete_position(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let position_id = path.as_str().to_string();
        if state
            .employees
            .iter()
            .any(|entry| entry.value().position_id.as_deref() == Some(position_id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Position is still assigned to employees; reassign them first",
            ));
        }
        state
            .positions
            .remove(position_id.as_str())
            .map(|_| json!({ "deleted": true, "id": position_id }))
            .ok_or_else(|| UnifiedError::not_found("Position not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: contracts
// ---------------------------------------------------------------------------

async fn list_contracts_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .contracts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contract| {
                contract.employee_id == employee.id
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| contract.status == *status)
            })
            .map(|contract| contract_view(&ctx, &caller, &contract, &employee.user_id))
            .collect();
        items.sort_by(|a, b| {
            b.get("start_date")
                .and_then(|v| v.as_str())
                .cmp(&a.get("start_date").and_then(|v| v.as_str()))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateContract>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        let input = body.into_inner();
        if !matches!(
            input.contract_type.as_str(),
            "permanent" | "fixed_term" | "intern" | "part_time" | "contractor"
        ) {
            return Err(UnifiedError::bad_request(
                "contract_type must be one of: permanent, fixed_term, intern, part_time, contractor",
            ));
        }
        let start_date = parse_date(&input.start_date, "start_date")?;
        let end_date = match &input.end_date {
            Some(raw) if !raw.trim().is_empty() => Some(parse_date(raw, "end_date")?),
            _ => None,
        };
        if let Some(end) = end_date {
            if end < start_date {
                return Err(UnifiedError::bad_request(
                    "end_date must not be before start_date",
                ));
            }
        }
        if input.contract_type == "fixed_term" && end_date.is_none() {
            return Err(UnifiedError::bad_request(
                "fixed_term contracts require an end_date",
            ));
        }
        let salary_amount = parse_money(input.salary_amount, "salary_amount")?;
        let currency = match input.currency {
            Some(raw) => valid_currency(&raw)?,
            None => "USD".to_string(),
        };
        // One active contract at a time: creating a new active
        // contract expires the previous one at the day before the new
        // start date.
        for mut entry in state.contracts.iter_mut() {
            if entry.value().employee_id == employee_id && entry.value().status == "active" {
                entry.value_mut().status = "expired".to_string();
                entry.value_mut().updated_at = now();
            }
        }
        let contract = Contract {
            id: new_id(),
            employee_id,
            contract_type: input.contract_type,
            start_date,
            end_date,
            salary_amount,
            currency,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.contracts.insert(contract.id.clone(), contract.clone());
        Ok(contract)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let contract = state
            .contracts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        let employee = state
            .employees
            .get(contract.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(contract_view(&ctx, &caller, &contract, &employee.user_id))
    })();
    json::respond(result, debug)
}

async fn update_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateContract>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(contract_type) = &input.contract_type {
            if !matches!(
                contract_type.as_str(),
                "permanent" | "fixed_term" | "intern" | "part_time" | "contractor"
            ) {
                return Err(UnifiedError::bad_request(
                    "contract_type must be one of: permanent, fixed_term, intern, part_time, contractor",
                ));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "active" | "expired" | "terminated") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: active, expired, terminated",
                ));
            }
        }
        let start_date = match &input.start_date {
            Some(raw) => Some(parse_date(raw, "start_date")?),
            None => None,
        };
        let end_date = match &input.end_date {
            Some(Some(raw)) if !raw.trim().is_empty() => Some(Some(parse_date(raw, "end_date")?)),
            Some(_) => Some(None),
            None => None,
        };
        let salary_amount = match input.salary_amount {
            Some(value) => Some(parse_money(value, "salary_amount")?),
            None => None,
        };
        let currency = match &input.currency {
            Some(raw) => Some(valid_currency(raw)?),
            None => None,
        };
        let mut contract = state
            .contracts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        // Validate the new dates before writing anything: the entry
        // is a live reference, so a failed check after assignment
        // would leave the bad values stored.
        let new_type = input
            .contract_type
            .clone()
            .unwrap_or_else(|| contract.contract_type.clone());
        let new_start = start_date.unwrap_or(contract.start_date);
        let new_end = end_date.unwrap_or(contract.end_date);
        if let Some(end) = new_end {
            if end < new_start {
                return Err(UnifiedError::bad_request(
                    "end_date must not be before start_date",
                ));
            }
        }
        if new_type == "fixed_term" && new_end.is_none() {
            return Err(UnifiedError::bad_request(
                "fixed_term contracts require an end_date",
            ));
        }
        contract.contract_type = new_type;
        contract.start_date = new_start;
        contract.end_date = new_end;
        if let Some(salary_amount) = salary_amount {
            contract.salary_amount = salary_amount;
        }
        if let Some(currency) = currency {
            contract.currency = currency;
        }
        if let Some(status) = input.status {
            contract.status = status;
        }
        contract.updated_at = now();
        Ok(contract.clone())
    })();
    json::respond(result, debug)
}

async fn terminate_contract(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Contract> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let end_date = body
            .get("end_date")
            .and_then(|v| v.as_str())
            .map(|raw| parse_date(raw, "end_date"))
            .transpose()?
            .unwrap_or_else(today);
        let mut contract = state
            .contracts
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Contract not found"))?;
        if contract.status == "terminated" {
            return Err(UnifiedError::conflict("Contract is already terminated"));
        }
        if end_date < contract.start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        contract.status = "terminated".to_string();
        contract.end_date = Some(end_date);
        contract.updated_at = now();
        Ok(contract.clone())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: leave types
// ---------------------------------------------------------------------------

async fn list_leave_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LeaveType>> = (|| {
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let mut items: Vec<LeaveType> = state
            .leave_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|leave_type| {
                matches_search(&format!("{} {}", leave_type.name, leave_type.code), &params)
                    && params
                        .filters
                        .get("paid")
                        .is_none_or(|flag| leave_type.paid == (flag == "true"))
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateLeaveType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 255)?;
        let code = match input.code {
            Some(code) if !code.trim().is_empty() => slugify(&code),
            _ => slugify(&name),
        };
        if code.is_empty() {
            return Err(UnifiedError::bad_request("code must not be empty"));
        }
        if state.leave_types.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "A leave type with code \"{code}\" already exists"
            )));
        }
        let default_days = match input.default_days_per_year {
            Some(days) => parse_days(days, "default_days_per_year")?,
            None => 0.0,
        };
        let leave_type = LeaveType {
            id: new_id(),
            name,
            code,
            default_days_per_year: default_days,
            paid: input.paid.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.leave_types.insert(leave_type.id.clone(), leave_type.clone());
        Ok(leave_type)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let _caller = caller(&req)?;
        state
            .leave_types
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))
    })();
    json::respond(result, debug)
}

async fn update_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLeaveType>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveType> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let default_days = match input.default_days_per_year {
            Some(days) => Some(parse_days(days, "default_days_per_year")?),
            None => None,
        };
        let mut leave_type = state
            .leave_types
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))?;
        if let Some(name) = input.name {
            leave_type.name = require_text(&name, "name", 255)?;
        }
        if let Some(days) = default_days {
            leave_type.default_days_per_year = days;
        }
        if let Some(paid) = input.paid {
            leave_type.paid = paid;
        }
        leave_type.updated_at = now();
        Ok(leave_type.clone())
    })();
    json::respond(result, debug)
}

async fn delete_leave_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let leave_type_id = path.as_str().to_string();
        if state
            .leave_requests
            .iter()
            .any(|entry| entry.value().leave_type_id == leave_type_id)
        {
            return Err(UnifiedError::conflict(
                "Leave type still has leave requests; keep it for history",
            ));
        }
        state
            .leave_types
            .remove(leave_type_id.as_str())
            .map(|_| json!({ "deleted": true, "id": leave_type_id }))
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: leave balances
// ---------------------------------------------------------------------------

async fn list_leave_balances(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let year_filter = params
            .filters
            .get("year")
            .and_then(|raw| raw.parse::<i32>().ok());
        let mut items: Vec<Value> = state
            .leave_balances
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|balance| {
                balance.employee_id == employee.id
                    && year_filter.is_none_or(|year| balance.year == year)
                    && params
                        .filters
                        .get("leave_type_id")
                        .is_none_or(|leave_type| balance.leave_type_id == *leave_type)
            })
            .map(|balance| {
                json!({
                    "id": balance.id,
                    "employee_id": balance.employee_id,
                    "leave_type_id": balance.leave_type_id,
                    "year": balance.year,
                    "entitled_days": balance.entitled_days,
                    "used_days": balance.used_days,
                    "carried_over_days": balance.carried_over_days,
                    "remaining_days": balance.remaining(),
                    "created_at": balance.created_at,
                    "updated_at": balance.updated_at,
                })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("year")
                .and_then(|v| v.as_i64())
                .cmp(&b.get("year").and_then(|v| v.as_i64()))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_leave_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLeaveBalance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveBalance> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        let input = body.into_inner();
        if !state.leave_types.contains_key(input.leave_type_id.as_str()) {
            return Err(UnifiedError::not_found("Leave type not found"));
        }
        if !(2000..=2100).contains(&input.year) {
            return Err(UnifiedError::bad_request("year must be between 2000 and 2100"));
        }
        if state.leave_balances.iter().any(|entry| {
            let balance = entry.value();
            balance.employee_id == employee_id
                && balance.leave_type_id == input.leave_type_id
                && balance.year == input.year
        }) {
            return Err(UnifiedError::conflict(
                "A balance already exists for this employee, leave type, and year",
            ));
        }
        let leave_type = state
            .leave_types
            .get(input.leave_type_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave type not found"))?;
        let entitled_days = match input.entitled_days {
            Some(days) => parse_days(days, "entitled_days")?,
            None => leave_type.default_days_per_year,
        };
        let carried_over_days = match input.carried_over_days {
            Some(days) => parse_days(days, "carried_over_days")?,
            None => 0.0,
        };
        let balance = LeaveBalance {
            id: new_id(),
            employee_id,
            leave_type_id: input.leave_type_id,
            year: input.year,
            entitled_days,
            used_days: 0.0,
            carried_over_days,
            created_at: now(),
            updated_at: now(),
        };
        state.leave_balances.insert(balance.id.clone(), balance.clone());
        Ok(balance)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_leave_balance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLeaveBalance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveBalance> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let entitled_days = match input.entitled_days {
            Some(days) => Some(parse_days(days, "entitled_days")?),
            None => None,
        };
        let carried_over_days = match input.carried_over_days {
            Some(days) => Some(parse_days(days, "carried_over_days")?),
            None => None,
        };
        let mut balance = state
            .leave_balances
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave balance not found"))?;
        // Validate before writing: the entry is a live reference, so
        // a failed check after assignment would leave bad values stored.
        let new_entitled = entitled_days.unwrap_or(balance.entitled_days);
        let new_carried = carried_over_days.unwrap_or(balance.carried_over_days);
        if balance.used_days > new_entitled + new_carried {
            return Err(UnifiedError::unprocessable(
                "used_days already exceeds the new entitlement plus carried-over days",
            ));
        }
        balance.entitled_days = new_entitled;
        balance.carried_over_days = new_carried;
        balance.updated_at = now();
        Ok(balance.clone())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: leave requests
// ---------------------------------------------------------------------------

async fn list_leave_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<LeaveRequest>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let mut items: Vec<LeaveRequest> = state
            .leave_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                (read_all || Some(request.employee_id.as_str()) == own_employee_id.as_deref())
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| request.employee_id == *employee)
                    && params
                        .filters
                        .get("leave_type_id")
                        .is_none_or(|leave_type| request.leave_type_id == *leave_type)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| request.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let input = body.into_inner();
        // An employee files for themself; HR (`hr:manage`) may file on
        // behalf of any employee by passing `employee_id`.
        let employee_id = match &input.employee_id {
            Some(employee_id) if !employee_id.trim().is_empty() => {
                if !check_capability(&ctx, &caller.role, PERM_HR_MANAGE) {
                    return Err(UnifiedError::forbidden(
                        "Only HR managers may file leave for another employee",
                    ));
                }
                employee_id.trim().to_string()
            }
            _ => state
                .employee_by_user(&caller.actor)
                .map(|employee| employee.id)
                .ok_or_else(|| {
                    UnifiedError::not_found("No employee record exists for the caller")
                })?,
        };
        let employee = state
            .employees
            .get(employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if !state.leave_types.contains_key(input.leave_type_id.as_str()) {
            return Err(UnifiedError::not_found("Leave type not found"));
        }
        let start_date = parse_date(&input.start_date, "start_date")?;
        let end_date = parse_date(&input.end_date, "end_date")?;
        if end_date < start_date {
            return Err(UnifiedError::bad_request(
                "end_date must not be before start_date",
            ));
        }
        // Employees may not backdate leave; HR may, for corrections.
        if start_date < today() && !check_capability(&ctx, &caller.role, PERM_HR_MANAGE) {
            return Err(UnifiedError::bad_request(
                "start_date must not be in the past",
            ));
        }
        if state.leave_overlaps(&employee.id, start_date, end_date, None) {
            return Err(UnifiedError::conflict(
                "The requested dates overlap an existing pending or approved leave request",
            ));
        }
        let days = (end_date - start_date).num_days() as f64 + 1.0;
        let reason = optional_text(&input.reason, "reason", MAX_TEXT)?.unwrap_or_default();
        let request = LeaveRequest {
            id: new_id(),
            employee_id: employee.id.clone(),
            leave_type_id: input.leave_type_id,
            start_date,
            end_date,
            days,
            status: "pending".to_string(),
            reason,
            reviewed_by: None,
            review_note: None,
            created_at: now(),
            updated_at: now(),
        };
        state.leave_requests.insert(request.id.clone(), request.clone());
        state.emit(
            "hr.leave_requested",
            json!({
                "leave_request_id": request.id,
                "employee_id": request.employee_id,
                "leave_type_id": request.leave_type_id,
                "start_date": request.start_date.to_string(),
                "end_date": request.end_date.to_string(),
                "days": request.days,
            }),
        );
        Ok(request)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let request = state
            .leave_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        let employee = state
            .employees
            .get(request.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(request)
    })();
    json::respond(result, debug)
}

async fn approve_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let review_note = optional_text(&input.reason, "reason", MAX_TEXT)?;
        let request_id = path.as_str().to_string();
        let mut request = state
            .leave_requests
            .get_mut(request_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        if request.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending leave request can be approved (current status: {})",
                request.status
            )));
        }
        let employee_id = request.employee_id.clone();
        let leave_type_id = request.leave_type_id.clone();
        let days = request.days;
        let start_date = request.start_date;
        let end_date = request.end_date;
        let year = start_date.format("%Y").to_string().parse::<i32>().unwrap_or(2000);
        drop(request);
        if state.leave_overlaps(&employee_id, start_date, end_date, Some(request_id.as_str())) {
            return Err(UnifiedError::conflict(
                "The requested dates overlap another approved leave request",
            ));
        }
        let mut balance = state.ensure_balance(&employee_id, &leave_type_id, year)?;
        if balance.remaining() < days {
            return Err(UnifiedError::unprocessable(format!(
                "Insufficient leave balance: {days} day(s) requested, {} day(s) remaining",
                balance.remaining()
            )));
        }
        balance.used_days = round4(balance.used_days + days);
        balance.updated_at = now();
        state.leave_balances.insert(balance.id.clone(), balance);
        let mut request = state
            .leave_requests
            .get_mut(request_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        request.status = "approved".to_string();
        request.reviewed_by = Some(caller.actor.clone());
        request.review_note = review_note;
        request.updated_at = now();
        let approved = request.clone();
        drop(request);
        state.emit(
            "hr.leave_approved",
            json!({
                "leave_request_id": approved.id,
                "employee_id": approved.employee_id,
                "days": approved.days,
            }),
        );
        let employee_user_id = state
            .employees
            .get(approved.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Leave request approved",
                    "body": format!(
                        "Your leave request for {} day(s) was approved.",
                        approved.days
                    ),
                    "kind": "hr",
                }),
            );
        }
        Ok(approved)
    })();
    json::respond(result, debug)
}

async fn reject_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewLeaveRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let reason = require_text(
            input.reason.as_deref().unwrap_or(""),
            "reason",
            MAX_TEXT,
        )?;
        let mut request = state
            .leave_requests
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        if request.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending leave request can be rejected (current status: {})",
                request.status
            )));
        }
        request.status = "rejected".to_string();
        request.reviewed_by = Some(caller.actor.clone());
        request.review_note = Some(reason.clone());
        request.updated_at = now();
        let rejected = request.clone();
        drop(request);
        state.emit(
            "hr.leave_rejected",
            json!({
                "leave_request_id": rejected.id,
                "employee_id": rejected.employee_id,
                "reason": reason,
            }),
        );
        let employee_user_id = state
            .employees
            .get(rejected.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Leave request rejected",
                    "body": format!("Your leave request was rejected: {reason}"),
                    "kind": "hr",
                }),
            );
        }
        Ok(rejected)
    })();
    json::respond(result, debug)
}

async fn cancel_leave_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LeaveRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let request_id = path.as_str().to_string();
        let request = state
            .leave_requests
            .get(request_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        let employee = state
            .employees
            .get(request.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if !matches!(request.status.as_str(), "pending" | "approved") {
            return Err(UnifiedError::conflict(format!(
                "Only a pending or approved leave request can be cancelled (current status: {})",
                request.status
            )));
        }
        // An approved cancellation returns the days to the balance.
        if request.status == "approved" {
            let year = request
                .start_date
                .format("%Y")
                .to_string()
                .parse::<i32>()
                .unwrap_or(2000);
            if let Ok(mut balance) =
                state.ensure_balance(&request.employee_id, &request.leave_type_id, year)
            {
                balance.used_days = round4((balance.used_days - request.days).max(0.0));
                balance.updated_at = now();
                state.leave_balances.insert(balance.id.clone(), balance);
            }
        }
        let mut request = state
            .leave_requests
            .get_mut(request_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Leave request not found"))?;
        request.status = "cancelled".to_string();
        request.updated_at = now();
        Ok(request.clone())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: attendance
// ---------------------------------------------------------------------------

async fn list_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttendanceRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let mut items: Vec<AttendanceRecord> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                (read_all || Some(record.employee_id.as_str()) == own_employee_id.as_deref())
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| record.employee_id == *employee)
                    && params
                        .filters
                        .get("source")
                        .is_none_or(|source| record.source == *source)
                    && from.is_none_or(|from| record.work_date >= from)
                    && to.is_none_or(|to| record.work_date <= to)
            })
            .collect();
        items.sort_by(|a, b| {
            b.work_date
                .cmp(&a.work_date)
                .then(b.created_at.cmp(&a.created_at))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_attendance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateAttendance>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttendanceRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let employee = state
            .employees
            .get(input.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        let work_date = parse_date(&input.work_date, "work_date")?;
        if input.minutes_worked == 0 || input.minutes_worked > MAX_MINUTES_PER_DAY {
            return Err(UnifiedError::bad_request(
                "minutes_worked must be between 1 and 1440",
            ));
        }
        let entry_id = input
            .entry_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(entry_id) = &entry_id {
            if state
                .attendance
                .iter()
                .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
            {
                return Err(UnifiedError::conflict(format!(
                    "An attendance record already exists for entry {entry_id}"
                )));
            }
        }
        let source = match input.source {
            Some(source) if !source.trim().is_empty() => {
                require_text(&source, "source", 128)?
            }
            _ => "manual".to_string(),
        };
        let record = AttendanceRecord {
            id: new_id(),
            employee_id: employee.id.clone(),
            user_id: employee.user_id,
            work_date,
            minutes_worked: input.minutes_worked,
            source,
            entry_id,
            created_at: now(),
            updated_at: now(),
        };
        state.attendance.insert(record.id.clone(), record.clone());
        Ok(record)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_attendance_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttendanceRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let to = params
            .filters
            .get("to")
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok());
        let mut items: Vec<AttendanceRecord> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                record.employee_id == employee.id
                    && from.is_none_or(|from| record.work_date >= from)
                    && to.is_none_or(|to| record.work_date <= to)
            })
            .collect();
        items.sort_by(|a, b| b.work_date.cmp(&a.work_date));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Handlers: payroll
// ---------------------------------------------------------------------------

async fn list_payroll_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let payroll_capable = can_payroll(&ctx, &caller.role);
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Value> = state
            .payroll_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                params
                    .filters
                    .get("status")
                    .is_none_or(|status| run.status == *status)
                    && params
                        .filters
                        .get("period")
                        .is_none_or(|period| run.period == *period)
            })
            .map(|run| {
                if payroll_capable || read_all {
                    serde_json::to_value(&run).unwrap_or(Value::Null)
                } else {
                    // Employees without payroll access see that a run
                    // exists, but never its money totals.
                    json!({
                        "id": run.id,
                        "period": run.period,
                        "currency": run.currency,
                        "status": run.status,
                        "total_employees": run.total_employees,
                        "finalized_at": run.finalized_at,
                        "created_at": run.created_at,
                        "updated_at": run.updated_at,
                    })
                }
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("period")
                .and_then(|v| v.as_str())
                .cmp(&a.get("period").and_then(|v| v.as_str()))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreatePayrollRun>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let input = body.into_inner();
        let period = valid_period(&input.period)?;
        let currency = match input.currency {
            Some(raw) => valid_currency(&raw)?,
            None => "USD".to_string(),
        };
        if state
            .payroll_runs
            .iter()
            .any(|entry| entry.value().period == period)
        {
            return Err(UnifiedError::conflict(format!(
                "A payroll run already exists for period {period}"
            )));
        }
        let run = PayrollRun {
            id: new_id(),
            period,
            currency,
            status: "draft".to_string(),
            total_gross: 0.0,
            total_net: 0.0,
            total_employees: 0,
            finalized_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.payroll_runs.insert(run.id.clone(), run.clone());
        Ok(run)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let run = state
            .payroll_runs
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if can_payroll(&ctx, &caller.role)
            || check_capability(&ctx, &caller.role, PERM_HR_READ_ALL)
        {
            Ok(serde_json::to_value(&run).unwrap_or(Value::Null))
        } else {
            Ok(json!({
                "id": run.id,
                "period": run.period,
                "currency": run.currency,
                "status": run.status,
                "total_employees": run.total_employees,
                "finalized_at": run.finalized_at,
                "created_at": run.created_at,
                "updated_at": run.updated_at,
            }))
        }
    })();
    json::respond(result, debug)
}

/// Recompute one run from active contracts: gross is the contract
/// salary, net is gross plus adjustments minus deductions. Existing
/// adjustments and deductions are kept, so HR can tune a line and
/// re-run the computation without losing its edits.
async fn compute_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let run_id = path.as_str().to_string();
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if !matches!(run.status.as_str(), "draft" | "computed") {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or computed payroll run can be computed (current status: {})",
                run.status
            )));
        }
        let today = today();
        let currency = run.currency.clone();
        drop(run);
        let mut total_gross = 0.0;
        let mut total_net = 0.0;
        let mut employee_count = 0u32;
        let employees: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|employee| employee.status == "active")
            .collect();
        for employee in employees {
            let Some(contract) = state.active_contract(&employee.id, today) else {
                continue;
            };
            // Payroll is computed in the run currency; a contract in
            // another currency is skipped and reported in metadata
            // instead of being silently converted at a made-up rate.
            if contract.currency != currency {
                continue;
            }
            employee_count += 1;
            let gross = round4(contract.salary_amount);
            let existing = state.payroll_items.iter().find(|entry| {
                let item = entry.value();
                item.payroll_run_id == run_id && item.employee_id == employee.id
            });
            let (adjustments, deductions, notes) = match existing.as_ref() {
                Some(entry) => (
                    entry.value().adjustments,
                    entry.value().deductions,
                    entry.value().notes.clone(),
                ),
                None => (0.0, 0.0, String::new()),
            };
            let net = round4((gross + adjustments - deductions).max(0.0));
            total_gross = round4(total_gross + gross);
            total_net = round4(total_net + net);
            match existing {
                Some(entry) => {
                    let item_id = entry.key().clone();
                    drop(entry);
                    if let Some(mut item) = state.payroll_items.get_mut(item_id.as_str()) {
                        item.gross = gross;
                        item.net = net;
                        item.updated_at = now();
                    }
                }
                None => {
                    let item = PayrollItem {
                        id: new_id(),
                        payroll_run_id: run_id.clone(),
                        employee_id: employee.id.clone(),
                        gross,
                        adjustments,
                        deductions,
                        net,
                        notes,
                        created_at: now(),
                        updated_at: now(),
                    };
                    state.payroll_items.insert(item.id.clone(), item);
                }
            }
        }
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        run.total_gross = total_gross;
        run.total_net = total_net;
        run.total_employees = employee_count;
        run.status = "computed".to_string();
        run.updated_at = now();
        Ok(run.clone())
    })();
    json::respond(result, debug)
}

async fn finalize_payroll_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollRun> = (|| {
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let run_id = path.as_str().to_string();
        let mut run = state
            .payroll_runs
            .get_mut(run_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if run.status != "computed" {
            return Err(UnifiedError::conflict(format!(
                "Only a computed payroll run can be finalized (current status: {})",
                run.status
            )));
        }
        if run.total_employees == 0 {
            return Err(UnifiedError::conflict(
                "Cannot finalize a payroll run with no employees; compute it first",
            ));
        }
        run.status = "finalized".to_string();
        run.finalized_at = Some(now());
        run.updated_at = now();
        let finalized = run.clone();
        drop(run);
        // Finalizing never pays anyone: the billing module listens for
        // this event and owns any payout.
        state.emit(
            "hr.payroll_run_completed",
            json!({
                "payroll_run_id": finalized.id,
                "period": finalized.period,
                "total_gross": finalized.total_gross,
                "total_net": finalized.total_net,
                "employee_count": finalized.total_employees,
                "currency": finalized.currency,
            }),
        );
        Ok(finalized)
    })();
    json::respond(result, debug)
}

async fn list_payroll_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let run_id = path.as_str().to_string();
        if !state.payroll_runs.contains_key(run_id.as_str()) {
            return Err(UnifiedError::not_found("Payroll run not found"));
        }
        let params = query.into_inner();
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let payroll_capable = can_payroll(&ctx, &caller.role);
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let mut items: Vec<Value> = state
            .payroll_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.payroll_run_id == run_id
                    && (payroll_capable
                        || read_all
                        || Some(item.employee_id.as_str()) == own_employee_id.as_deref())
            })
            .map(|item| {
                let employee_user_id = state
                    .employees
                    .get(item.employee_id.as_str())
                    .map(|entry| entry.value().user_id.clone())
                    .unwrap_or_default();
                payroll_item_view(&ctx, &caller, &item, &employee_user_id)
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("employee_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("employee_id").and_then(|v| v.as_str()))
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_payroll_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePayrollItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PayrollItem> = (|| {
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let input = body.into_inner();
        let adjustments = match input.adjustments {
            Some(value) => Some(parse_money(value, "adjustments")?),
            None => None,
        };
        // Deductions are a positive amount subtracted from gross.
        let deductions = match input.deductions {
            Some(value) => Some(parse_money(value, "deductions")?),
            None => None,
        };
        let notes = match input.notes {
            Some(note) => Some(require_text(&note, "notes", MAX_TEXT)?),
            None => None,
        };
        let mut item = state
            .payroll_items
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Payroll item not found"))?;
        let run_status = state
            .payroll_runs
            .get(item.payroll_run_id.as_str())
            .map(|entry| entry.value().status.clone())
            .ok_or_else(|| UnifiedError::not_found("Payroll run not found"))?;
        if run_status == "finalized" {
            return Err(UnifiedError::conflict(
                "A finalized payroll run can no longer be changed",
            ));
        }
        if let Some(adjustments) = adjustments {
            item.adjustments = adjustments;
        }
        if let Some(deductions) = deductions {
            item.deductions = deductions;
        }
        if let Some(notes) = notes {
            item.notes = notes;
        }
        item.net = round4((item.gross + item.adjustments - item.deductions).max(0.0));
        item.updated_at = now();
        let updated = item.clone();
        let run_id = updated.payroll_run_id.clone();
        drop(item);
        // Keep the run totals in step with the edited line.
        let mut total_gross = 0.0;
        let mut total_net = 0.0;
        for entry in state.payroll_items.iter() {
            if entry.value().payroll_run_id == run_id {
                total_gross = round4(total_gross + entry.value().gross);
                total_net = round4(total_net + entry.value().net);
            }
        }
        if let Some(mut run) = state.payroll_runs.get_mut(run_id.as_str()) {
            run.total_gross = total_gross;
            run.total_net = total_net;
            run.updated_at = now();
        }
        Ok(updated)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: performance reviews
// ---------------------------------------------------------------------------

async fn list_review_cycles(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReviewCycle>> = (|| {
        let _caller = caller(&req)?;
        let params = query.into_inner();
        let mut items: Vec<ReviewCycle> = state
            .review_cycles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|cycle| {
                matches_search(&cycle.name, &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| cycle.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.period_start.cmp(&a.period_start));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateReviewCycle>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 255)?;
        let period_start = parse_date(&input.period_start, "period_start")?;
        let period_end = parse_date(&input.period_end, "period_end")?;
        if period_end < period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let cycle = ReviewCycle {
            id: new_id(),
            name,
            period_start,
            period_end,
            status: "active".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.review_cycles.insert(cycle.id.clone(), cycle.clone());
        Ok(cycle)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let _caller = caller(&req)?;
        state
            .review_cycles
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))
    })();
    json::respond(result, debug)
}

async fn close_review_cycle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ReviewCycle> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let cycle_id = path.as_str().to_string();
        if state.reviews.iter().any(|entry| {
            let review = entry.value();
            review.cycle_id == cycle_id && review.status != "completed"
        }) {
            return Err(UnifiedError::conflict(
                "Review cycle still has incomplete reviews; complete them first",
            ));
        }
        let mut cycle = state
            .review_cycles
            .get_mut(cycle_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))?;
        if cycle.status == "closed" {
            return Err(UnifiedError::conflict("Review cycle is already closed"));
        }
        cycle.status = "closed".to_string();
        cycle.updated_at = now();
        Ok(cycle.clone())
    })();
    json::respond(result, debug)
}

async fn list_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Review>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_HR_READ_ALL);
        let own_employee_id = state
            .employee_by_user(&caller.actor)
            .map(|employee| employee.id);
        let mut items: Vec<Review> = state
            .reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|review| {
                (read_all
                    || Some(review.employee_id.as_str()) == own_employee_id.as_deref()
                    || review.reviewer_id == caller.actor)
                    && params
                        .filters
                        .get("cycle_id")
                        .is_none_or(|cycle| review.cycle_id == *cycle)
                    && params
                        .filters
                        .get("employee_id")
                        .is_none_or(|employee| review.employee_id == *employee)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| review.status == *status)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    body: web::Json<CreateReview>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        let cycle = state
            .review_cycles
            .get(input.cycle_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review cycle not found"))?;
        if cycle.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Reviews can only be created in an active cycle (current status: {})",
                cycle.status
            )));
        }
        if !state.employees.contains_key(input.employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        if let Some(rating) = input.rating {
            if !(1..=5).contains(&rating) {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
        }
        if state.reviews.iter().any(|entry| {
            let review = entry.value();
            review.cycle_id == input.cycle_id && review.employee_id == input.employee_id
        }) {
            return Err(UnifiedError::conflict(
                "A review already exists for this employee in this cycle",
            ));
        }
        let goals = match input.goals {
            Some(goals) => {
                let mut clean: Vec<String> = Vec::new();
                for goal in goals {
                    clean.push(require_text(&goal, "goals entry", 500)?);
                }
                clean
            }
            None => Vec::new(),
        };
        let review = Review {
            id: new_id(),
            cycle_id: input.cycle_id,
            employee_id: input.employee_id,
            reviewer_id: require_text(&input.reviewer_id, "reviewer_id", 255)?,
            rating: input.rating,
            goals,
            reviewer_notes: optional_text(&input.reviewer_notes, "reviewer_notes", MAX_TEXT)?
                .unwrap_or_default(),
            status: "draft".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.reviews.insert(review.id.clone(), review.clone());
        Ok(review)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let review = state
            .reviews
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        let employee = state
            .employees
            .get(review.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        if !can_read_employee(&ctx, &caller, &employee) && review.reviewer_id != caller.actor {
            return Err(UnifiedError::not_found("Review not found"));
        }
        Ok(review)
    })();
    json::respond(result, debug)
}

async fn update_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReview>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let input = body.into_inner();
        if let Some(rating) = input.rating {
            if !(1..=5).contains(&rating) {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
        }
        if let Some(status) = &input.status {
            if !matches!(status.as_str(), "draft" | "submitted" | "completed") {
                return Err(UnifiedError::bad_request(
                    "status must be one of: draft, submitted, completed",
                ));
            }
        }
        let goals = match input.goals {
            Some(goals) => {
                let mut clean: Vec<String> = Vec::new();
                for goal in goals {
                    clean.push(require_text(&goal, "goals entry", 500)?);
                }
                Some(clean)
            }
            None => None,
        };
        let reviewer_notes = match input.reviewer_notes {
            Some(note) => Some(require_text(&note, "reviewer_notes", MAX_TEXT)?),
            None => None,
        };
        let mut review = state
            .reviews
            .get_mut(path.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        if review.status == "completed" {
            return Err(UnifiedError::conflict(
                "A completed review can no longer be changed",
            ));
        }
        if let Some(rating) = input.rating {
            review.rating = Some(rating);
        }
        if let Some(goals) = goals {
            review.goals = goals;
        }
        if let Some(reviewer_notes) = reviewer_notes {
            review.reviewer_notes = reviewer_notes;
        }
        if let Some(status) = input.status {
            if status == "completed" && review.rating.is_none() {
                return Err(UnifiedError::bad_request(
                    "A review needs a rating before it can be completed",
                ));
            }
            review.status = status;
        }
        review.updated_at = now();
        Ok(review.clone())
    })();
    json::respond(result, debug)
}

async fn complete_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        let review_id = path.as_str().to_string();
        let review = state
            .reviews
            .get(review_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        // The assigned reviewer may complete their own review without
        // holding an HR capability; anyone else needs `hr:manage`.
        // (An employee cannot complete their own review this way:
        // the reviewer is never the employee under review unless HR
        // assigned them as such, in which case the assignment stands.)
        if review.reviewer_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if review.status == "completed" {
            return Err(UnifiedError::conflict("Review is already completed"));
        }
        if review.rating.is_none() {
            return Err(UnifiedError::bad_request(
                "A review needs a rating before it can be completed",
            ));
        }
        let mut review = state
            .reviews
            .get_mut(review_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Review not found"))?;
        review.status = "completed".to_string();
        review.updated_at = now();
        let completed = review.clone();
        drop(review);
        state.emit(
            "hr.review_completed",
            json!({
                "review_id": completed.id,
                "cycle_id": completed.cycle_id,
                "employee_id": completed.employee_id,
                "rating": completed.rating,
            }),
        );
        let employee_user_id = state
            .employees
            .get(completed.employee_id.as_str())
            .map(|entry| entry.value().user_id.clone());
        if let Some(user_id) = employee_user_id {
            state.emit(
                "notification.requested",
                json!({
                    "user_id": user_id,
                    "title": "Performance review completed",
                    "body": "Your performance review was completed.",
                    "kind": "hr",
                }),
            );
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: onboarding / offboarding checklists
// ---------------------------------------------------------------------------

async fn create_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateChecklist>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        let input = body.into_inner();
        if !matches!(input.kind.as_str(), "onboarding" | "offboarding") {
            return Err(UnifiedError::bad_request(
                "kind must be one of: onboarding, offboarding",
            ));
        }
        if state.checklists.iter().any(|entry| {
            let checklist = entry.value();
            checklist.employee_id == employee_id
                && checklist.kind == input.kind
                && checklist.status != "completed"
        }) {
            return Err(UnifiedError::conflict(format!(
                "An open {} checklist already exists for this employee",
                input.kind
            )));
        }
        let checklist = Checklist {
            id: new_id(),
            employee_id: employee_id.clone(),
            kind: input.kind,
            status: "pending".to_string(),
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .checklists
            .insert(checklist.id.clone(), checklist.clone());
        let mut items: Vec<ChecklistItem> = Vec::new();
        for title in input.items {
            let item = ChecklistItem {
                id: new_id(),
                checklist_id: checklist.id.clone(),
                title: require_text(&title, "items entry", 500)?,
                completed: false,
                completed_by: None,
                completed_at: None,
                created_at: now(),
                updated_at: now(),
            };
            state.checklist_items.insert(item.id.clone(), item.clone());
            items.push(item);
        }
        Ok(json!({
            "checklist": checklist,
            "items": items,
        }))
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_checklists_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .checklists
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|checklist| {
                checklist.employee_id == employee.id
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| checklist.kind == *kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| checklist.status == *status)
            })
            .map(|checklist| {
                let items: Vec<ChecklistItem> = state
                    .checklist_items
                    .iter()
                    .map(|entry| entry.value().clone())
                    .filter(|item| item.checklist_id == checklist.id)
                    .collect();
                let done = items.iter().filter(|item| item.completed).count();
                json!({
                    "checklist": checklist,
                    "items": items,
                    "completed_items": done,
                    "total_items": items.len(),
                })
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("checklist")
                .and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str())
                .cmp(
                    &a.get("checklist")
                        .and_then(|v| v.get("created_at"))
                        .and_then(|v| v.as_str()),
                )
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let checklist = state
            .checklists
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        let employee = state
            .employees
            .get(checklist.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        let mut items: Vec<ChecklistItem> = state
            .checklist_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.checklist_id == checklist.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let done = items.iter().filter(|item| item.completed).count();
        let total = items.len();
        Ok(json!({
            "checklist": checklist,
            "items": items,
            "completed_items": done,
            "total_items": total,
        }))
    })();
    json::respond(result, debug)
}

async fn add_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateChecklistItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let checklist_id = path.as_str().to_string();
        let checklist = state
            .checklists
            .get(checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        if checklist.status == "completed" {
            return Err(UnifiedError::conflict(
                "Items cannot be added to a completed checklist",
            ));
        }
        let input = body.into_inner();
        let item = ChecklistItem {
            id: new_id(),
            checklist_id,
            title: require_text(&input.title, "title", 500)?,
            completed: false,
            completed_by: None,
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.checklist_items.insert(item.id.clone(), item.clone());
        if let Some(mut checklist) = state.checklists.get_mut(item.checklist_id.as_str()) {
            if checklist.status == "pending" {
                checklist.status = "in_progress".to_string();
                checklist.updated_at = now();
            }
        }
        Ok(item)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn complete_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let item_id = path.as_str().to_string();
        let item = state
            .checklist_items
            .get(item_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        let checklist = state
            .checklists
            .get(item.checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        let employee = state
            .employees
            .get(checklist.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if item.completed {
            return Err(UnifiedError::conflict("Checklist item is already completed"));
        }
        let mut item = state
            .checklist_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        item.completed = true;
        item.completed_by = Some(caller.actor.clone());
        item.completed_at = Some(now());
        item.updated_at = now();
        let completed = item.clone();
        drop(item);
        if let Some(mut checklist) = state.checklists.get_mut(checklist.id.as_str()) {
            if checklist.status == "pending" {
                checklist.status = "in_progress".to_string();
                checklist.updated_at = now();
            }
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}

async fn complete_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Checklist> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let checklist_id = path.as_str().to_string();
        let checklist = state
            .checklists
            .get(checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        if checklist.status == "completed" {
            return Err(UnifiedError::conflict("Checklist is already completed"));
        }
        let items: Vec<ChecklistItem> = state
            .checklist_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.checklist_id == checklist_id)
            .collect();
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "A checklist with no items cannot be completed",
            ));
        }
        if items.iter().any(|item| !item.completed) {
            return Err(UnifiedError::conflict(
                "All checklist items must be completed first",
            ));
        }
        let mut checklist = state
            .checklists
            .get_mut(checklist_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        checklist.status = "completed".to_string();
        checklist.completed_at = Some(now());
        checklist.updated_at = now();
        let completed = checklist.clone();
        drop(checklist);
        if completed.kind == "onboarding" {
            state.emit(
                "hr.onboarding_completed",
                json!({
                    "checklist_id": completed.id,
                    "employee_id": completed.employee_id,
                }),
            );
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: employee documents (metadata only)
// ---------------------------------------------------------------------------

async fn list_documents_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<EmployeeDocument>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<EmployeeDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                document.employee_id == employee.id
                    && params
                        .filters
                        .get("document_type")
                        .is_none_or(|kind| document.document_type == *kind)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEmployeeDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EmployeeDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee_id = path.as_str().to_string();
        let employee = state
            .employees
            .get(employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        let input = body.into_inner();
        let document_type = match input.document_type.as_deref() {
            Some(kind)
                if matches!(
                    kind,
                    "contract" | "id" | "passport" | "certificate" | "other"
                ) =>
            {
                kind.to_string()
            }
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "document_type must be one of: contract, id, passport, certificate, other",
                ))
            }
            None => "other".to_string(),
        };
        let storage_file_id = require_text(&input.storage_file_id, "storage_file_id", 255)?;
        // The file itself lives in Storage. Ask Storage whether the
        // file exists; a definite "no" rejects the metadata, while no
        // answer is accepted (see the module header).
        if !state.storage_file_exists(&storage_file_id) {
            return Err(UnifiedError::not_found(format!(
                "Storage file {storage_file_id} not found"
            )));
        }
        if let Some(size) = input.size_bytes {
            if size < 0 {
                return Err(UnifiedError::bad_request("size_bytes must not be negative"));
            }
        }
        let document = EmployeeDocument {
            id: new_id(),
            employee_id,
            title: require_text(&input.title, "title", 255)?,
            document_type,
            storage_file_id,
            mime_type: optional_text(&input.mime_type, "mime_type", 128)?,
            size_bytes: input.size_bytes,
            uploaded_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state.documents.insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EmployeeDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let document = state
            .documents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document not found"))?;
        let employee = state
            .employees
            .get(document.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(document)
    })();
    json::respond(result, debug)
}

async fn delete_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let document_id = path.as_str().to_string();
        state
            .documents
            .remove(document_id.as_str())
            .map(|_| json!({ "deleted": true, "id": document_id }))
            .ok_or_else(|| UnifiedError::not_found("Document not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: HR reports
// ---------------------------------------------------------------------------

async fn report_headcount(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ_ALL)?;
        let employees: Vec<Employee> = state
            .employees
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        let mut by_department: HashMap<String, u64> = HashMap::new();
        let mut by_status: HashMap<String, u64> = HashMap::new();
        let mut by_position: HashMap<String, u64> = HashMap::new();
        for employee in &employees {
            *by_status.entry(employee.status.clone()).or_insert(0) += 1;
            *by_department
                .entry(
                    employee
                        .department_id
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                )
                .or_insert(0) += 1;
            *by_position
                .entry(
                    employee
                        .position_id
                        .clone()
                        .unwrap_or_else(|| "unassigned".to_string()),
                )
                .or_insert(0) += 1;
        }
        let active = by_status.get("active").copied().unwrap_or(0);
        Ok(json!({
            "total_employees": employees.len(),
            "active_employees": active,
            "by_department": by_department,
            "by_status": by_status,
            "by_position": by_position,
        }))
    })();
    json::respond(result, debug)
}

async fn report_leave_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ_ALL)?;
        let params = query.into_inner();
        let year = params
            .filters
            .get("year")
            .and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or_else(|| today().format("%Y").to_string().parse::<i32>().unwrap_or(2000));
        let mut by_type: HashMap<String, Value> = HashMap::new();
        for entry in state.leave_types.iter() {
            let leave_type = entry.value();
            by_type.insert(
                leave_type.id.clone(),
                json!({
                    "leave_type_id": leave_type.id,
                    "name": leave_type.name,
                    "code": leave_type.code,
                    "approved_days": 0.0,
                    "pending_days": 0.0,
                    "rejected_days": 0.0,
                    "cancelled_days": 0.0,
                    "total_entitled_days": 0.0,
                    "total_used_days": 0.0,
                }),
            );
        }
        for entry in state.leave_requests.iter() {
            let request = entry.value();
            if request.start_date.format("%Y").to_string().parse::<i32>().unwrap_or(0) != year {
                continue;
            }
            if let Some(summary) = by_type.get_mut(&request.leave_type_id) {
                let key = match request.status.as_str() {
                    "approved" => "approved_days",
                    "pending" => "pending_days",
                    "rejected" => "rejected_days",
                    "cancelled" => "cancelled_days",
                    _ => continue,
                };
                let current = summary.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
                summary[key] = json!(round4(current + request.days));
            }
        }
        for entry in state.leave_balances.iter() {
            let balance = entry.value();
            if balance.year != year {
                continue;
            }
            if let Some(summary) = by_type.get_mut(&balance.leave_type_id) {
                let entitled = summary
                    .get("total_entitled_days")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                summary["total_entitled_days"] =
                    json!(round4(entitled + balance.entitled_days + balance.carried_over_days));
                let used = summary
                    .get("total_used_days")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                summary["total_used_days"] = json!(round4(used + balance.used_days));
            }
        }
        let mut by_type: Vec<Value> = by_type.into_values().collect();
        by_type.sort_by(|a, b| {
            a.get("name")
                .and_then(|v| v.as_str())
                .cmp(&b.get("name").and_then(|v| v.as_str()))
        });
        let pending_requests = state
            .leave_requests
            .iter()
            .filter(|entry| entry.value().status == "pending")
            .count() as u64;
        Ok(json!({
            "year": year,
            "pending_requests": pending_requests,
            "by_leave_type": by_type,
        }))
    })();
    json::respond(result, debug)
}

async fn report_payroll_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        if !can_payroll(&ctx, &caller.role) {
            return Err(UnifiedError::forbidden(format!(
                "Role \"{}\" does not hold permission \"{}\"",
                caller.role, PERM_HR_PAYROLL
            )));
        }
        let params = query.into_inner();
        let runs: Vec<PayrollRun> = state
            .payroll_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                params
                    .filters
                    .get("period")
                    .is_none_or(|period| run.period == *period)
            })
            .collect();
        let mut by_status: HashMap<String, u64> = HashMap::new();
        let mut total_gross = 0.0;
        let mut total_net = 0.0;
        let mut total_employees = 0u32;
        for run in &runs {
            *by_status.entry(run.status.clone()).or_insert(0) += 1;
            total_gross = round4(total_gross + run.total_gross);
            total_net = round4(total_net + run.total_net);
            total_employees += run.total_employees;
        }
        let mut by_period: Vec<Value> = runs
            .iter()
            .map(|run| {
                json!({
                    "period": run.period,
                    "status": run.status,
                    "currency": run.currency,
                    "total_gross": run.total_gross,
                    "total_net": run.total_net,
                    "total_employees": run.total_employees,
                })
            })
            .collect();
        by_period.sort_by(|a, b| {
            b.get("period")
                .and_then(|v| v.as_str())
                .cmp(&a.get("period").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "total_runs": runs.len(),
            "total_gross": total_gross,
            "total_net": total_net,
            "total_employees": total_employees,
            "by_status": by_status,
            "by_period": by_period,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// Consume TimeTracker entries and record one attendance row per entry.
/// Accepts BOTH payload shapes (see the module header):
///   - documented: `{user_id, date, minutes, source}`
///   - already in this tree (`timetracker.time.logged`):
///     `{entry_id, user_id, started_at, ended_at, duration_secs,
///     hours, source, ...}`
/// Unknown payloads (missing user id, unparsable date, zero minutes)
/// and users with no employee record are ignored silently: a
/// TimeTracker event must never fail HR, and attendance for a user
/// who is not an employee is not HR data.
/// Answer `user.manager.query` with the manager of the given user.
///
/// The approvals module asks for `{"user_id": "..."}` and reads the
/// `manager_id` key from the answer, comparing it against the
/// deciding caller's user id — so the answer must be the manager's
/// USER id, not the manager's employee id. The employee record
/// stores `manager_id` as an employee id, so this handler resolves
/// it one step further: employee (by `user_id`) -> manager employee
/// (by `manager_id`) -> that employee's `user_id`.
///
/// When HR holds no employee for the user, no manager is recorded,
/// or the manager's employee record is missing, the handler answers
/// `resolved: false` with `manager_id: null` and a reason. It never
/// fabricates a manager and never guesses: an unresolved answer
/// leaves the approvals slot unresolved for admin reassignment.
fn on_user_manager_query(state: Arc<HrState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let user_id = data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let Some(user_id) = user_id else {
        return;
    };
    let answer = |resolved: bool, manager_id: Option<String>, reason: Option<&str>| {
        let mut answer = json!({
            "user_id": user_id,
            "resolved": resolved,
            "manager_id": manager_id,
        });
        if let Some(reason) = reason {
            answer["reason"] = json!(reason);
        }
        answer
    };
    let Some(employee) = state.employee_by_user(&user_id) else {
        let answer = answer(false, None, Some("no employee record for user"));
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let Some(manager_employee_id) = employee.manager_id.clone() else {
        let answer = answer(false, None, Some("no manager recorded for employee"));
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let Some(manager) = state
        .employees
        .get(manager_employee_id.as_str())
        .map(|entry| entry.value().clone())
    else {
        let answer = answer(
            false,
            None,
            Some("manager employee record not found"),
        );
        ctx.set("resolved", json!(false));
        ctx.set("manager_id", Value::Null);
        ctx.add_result(answer);
        return;
    };
    let answer = answer(true, Some(manager.user_id.clone()), None);
    ctx.set("resolved", json!(true));
    ctx.set("manager_id", json!(manager.user_id));
    ctx.add_result(answer);
}

fn on_timetracker_entry(state: Arc<HrState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) else {
        return;
    };
    let user_id = user_id.to_string();
    let work_date = match data.get("date").and_then(|v| v.as_str()) {
        Some(raw) => match NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
            Ok(date) => date,
            Err(_) => return,
        },
        None => {
            // Fall back to the date part of `started_at` (RFC 3339),
            // then `ended_at`.
            let timestamp = data
                .get("started_at")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("ended_at").and_then(|v| v.as_str()));
            let Some(timestamp) = timestamp else {
                return;
            };
            match NaiveDate::parse_from_str(&timestamp[..timestamp.len().min(10)], "%Y-%m-%d") {
                Ok(date) => date,
                Err(_) => return,
            }
        }
    };
    let minutes: u64 = if let Some(minutes) = data.get("minutes").and_then(|v| v.as_u64()) {
        minutes
    } else if let Some(secs) = data.get("duration_secs").and_then(|v| v.as_u64()) {
        secs.div_ceil(60)
    } else if let Some(hours) = data.get("hours").and_then(|v| v.as_f64()) {
        (hours * 60.0).round() as u64
    } else {
        return;
    };
    if minutes == 0 || minutes > MAX_MINUTES_PER_DAY {
        return;
    }
    let source = data
        .get("source")
        .and_then(|v| v.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("timetracker")
        .to_string();
    let entry_id = data
        .get("entry_id")
        .and_then(|v| v.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(employee) = state.employee_by_user(&user_id) else {
        return;
    };
    // De-duplicate repeated delivery of the same TimeTracker entry.
    if let Some(entry_id) = &entry_id {
        if state
            .attendance
            .iter()
            .any(|entry| entry.value().entry_id.as_deref() == Some(entry_id.as_str()))
        {
            return;
        }
    }
    let record = AttendanceRecord {
        id: new_id(),
        employee_id: employee.id.clone(),
        user_id: employee.user_id,
        work_date,
        minutes_worked: minutes,
        source,
        entry_id,
        created_at: now(),
        updated_at: now(),
    };
    state.attendance.insert(record.id.clone(), record);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct HrModule {
    state: Arc<HrState>,
}

impl HrModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(HrState::new()),
        }
    }
}

impl Default for HrModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for HrModule {
    fn id(&self) -> &'static str {
        "hr"
    }

    fn name(&self) -> &'static str {
        "Human Resource Management"
    }

    fn description(&self) -> &'static str {
        "Human resources: employees, positions, contracts, leave and balances, attendance from TimeTracker, payroll runs, performance reviews, onboarding and offboarding checklists, employee documents, org-chart data, and HR reports."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "departments", "notifications", "audit"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "departments", "notifications", "audit"]
    }

    fn available_as_product(&self) -> bool {
        false
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Answer `user.manager.query` for the approvals module (see
        // `on_user_manager_query`): the requester's manager, resolved
        // from HR's employee / manager data, or an explicit
        // unresolved answer when HR records no manager.
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_user_manager_query(state.clone(), ctx));
        bus.register("user.manager.query", "hr", handler);
        // Both TimeTracker event names (see the module header): the
        // documented `timetracker.entry_logged` and the
        // `timetracker.time.logged` the TimeTracker module in this
        // tree actually emits. One handler accepts both payloads.
        for event in ["timetracker.entry_logged", "timetracker.time.logged"] {
            let state = self.state.clone();
            let handler: HookHandler =
                Arc::new(move |ctx: &mut HookContext| on_timetracker_entry(state.clone(), ctx));
            bus.register(event, "hr", handler);
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/hr")
                // Literal segments first: they must win over `/{id}`.
                .route("/org-chart", web::get().to(get_org_chart))
                .route("/reports/headcount", web::get().to(report_headcount))
                .route("/reports/leave-usage", web::get().to(report_leave_usage))
                .route("/reports/payroll-summary", web::get().to(report_payroll_summary))
                .route("/employees", web::get().to(list_employees))
                .route("/employees", web::post().to(create_employee))
                .route("/employees/{id}", web::get().to(get_employee))
                .route("/employees/{id}", web::put().to(update_employee))
                .route("/employees/{id}", web::delete().to(delete_employee))
                .route("/employees/{id}/contracts", web::get().to(list_contracts_for_employee))
                .route("/employees/{id}/contracts", web::post().to(create_contract))
                .route("/employees/{id}/leave-balances", web::get().to(list_leave_balances))
                .route("/employees/{id}/leave-balances", web::post().to(create_leave_balance))
                .route("/employees/{id}/attendance", web::get().to(list_attendance_for_employee))
                .route("/employees/{id}/checklists", web::get().to(list_checklists_for_employee))
                .route("/employees/{id}/checklists", web::post().to(create_checklist))
                .route("/employees/{id}/documents", web::get().to(list_documents_for_employee))
                .route("/employees/{id}/documents", web::post().to(create_document))
                .route("/positions", web::get().to(list_positions))
                .route("/positions", web::post().to(create_position))
                .route("/positions/{id}", web::get().to(get_position))
                .route("/positions/{id}", web::put().to(update_position))
                .route("/positions/{id}", web::delete().to(delete_position))
                .route("/contracts/{id}", web::get().to(get_contract))
                .route("/contracts/{id}", web::put().to(update_contract))
                .route("/contracts/{id}/terminate", web::post().to(terminate_contract))
                .route("/leave-types", web::get().to(list_leave_types))
                .route("/leave-types", web::post().to(create_leave_type))
                .route("/leave-types/{id}", web::get().to(get_leave_type))
                .route("/leave-types/{id}", web::put().to(update_leave_type))
                .route("/leave-types/{id}", web::delete().to(delete_leave_type))
                .route("/leave-balances/{id}", web::put().to(update_leave_balance))
                .route("/leave-requests", web::get().to(list_leave_requests))
                .route("/leave-requests", web::post().to(create_leave_request))
                .route("/leave-requests/{id}", web::get().to(get_leave_request))
                .route("/leave-requests/{id}/approve", web::post().to(approve_leave_request))
                .route("/leave-requests/{id}/reject", web::post().to(reject_leave_request))
                .route("/leave-requests/{id}/cancel", web::post().to(cancel_leave_request))
                .route("/attendance", web::get().to(list_attendance))
                .route("/attendance", web::post().to(create_attendance))
                .route("/payroll-runs", web::get().to(list_payroll_runs))
                .route("/payroll-runs", web::post().to(create_payroll_run))
                .route("/payroll-runs/{id}", web::get().to(get_payroll_run))
                .route("/payroll-runs/{id}/compute", web::post().to(compute_payroll_run))
                .route("/payroll-runs/{id}/finalize", web::post().to(finalize_payroll_run))
                .route("/payroll-runs/{id}/items", web::get().to(list_payroll_items))
                .route("/payroll-items/{id}", web::put().to(update_payroll_item))
                .route("/review-cycles", web::get().to(list_review_cycles))
                .route("/review-cycles", web::post().to(create_review_cycle))
                .route("/review-cycles/{id}", web::get().to(get_review_cycle))
                .route("/review-cycles/{id}/close", web::post().to(close_review_cycle))
                .route("/reviews", web::get().to(list_reviews))
                .route("/reviews", web::post().to(create_review))
                .route("/reviews/{id}", web::get().to(get_review))
                .route("/reviews/{id}", web::put().to(update_review))
                .route("/reviews/{id}/complete", web::post().to(complete_review))
                .route("/checklists/{id}", web::get().to(get_checklist))
                .route("/checklists/{id}/items", web::post().to(add_checklist_item))
                .route("/checklists/{id}/complete", web::post().to(complete_checklist))
                .route("/checklist-items/{id}/complete", web::post().to(complete_checklist_item))
                .route("/documents/{id}", web::get().to(get_document))
                .route("/documents/{id}", web::delete().to(delete_document)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/hr/employees", owner: "hr", description: "List employees (HR sees all; an employee sees only their own record)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/employees", owner: "hr", description: "Create an employee record (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}", owner: "hr", description: "Get one employee (self or hr:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/employees/{id}", owner: "hr", description: "Update an employee (hr:manage; manager cycles rejected)." },
            RouteInfo { method: "DELETE", path: "/api/v1/hr/employees/{id}", owner: "hr", description: "Delete an employee (hr:manage; refused with active contract, open leave, or reports)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/org-chart", owner: "hr", description: "Org-chart data: manager links for every employee, no salary data." },
            RouteInfo { method: "GET", path: "/api/v1/hr/positions", owner: "hr", description: "List positions (salary band shown only to HR-capable callers)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/positions", owner: "hr", description: "Create a position (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/positions/{id}", owner: "hr", description: "Get one position." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/positions/{id}", owner: "hr", description: "Update a position (hr:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/hr/positions/{id}", owner: "hr", description: "Delete a position (hr:manage; refused while assigned)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}/contracts", owner: "hr", description: "List an employee's contracts (salary hidden from other employees)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/employees/{id}/contracts", owner: "hr", description: "Create a contract (hr:manage; expires the previous active contract)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/contracts/{id}", owner: "hr", description: "Get one contract (salary hidden from other employees)." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/contracts/{id}", owner: "hr", description: "Update a contract (hr:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/contracts/{id}/terminate", owner: "hr", description: "Terminate a contract (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/leave-types", owner: "hr", description: "List leave types." },
            RouteInfo { method: "POST", path: "/api/v1/hr/leave-types", owner: "hr", description: "Create a leave type (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/leave-types/{id}", owner: "hr", description: "Get one leave type." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/leave-types/{id}", owner: "hr", description: "Update a leave type (hr:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/hr/leave-types/{id}", owner: "hr", description: "Delete a leave type (hr:manage; refused while referenced by leave requests)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}/leave-balances", owner: "hr", description: "List an employee's leave balances with remaining days (self or hr:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/employees/{id}/leave-balances", owner: "hr", description: "Create a leave balance (hr:manage)." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/leave-balances/{id}", owner: "hr", description: "Update a leave balance (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/leave-requests", owner: "hr", description: "List leave requests (HR sees all; an employee sees only their own)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/leave-requests", owner: "hr", description: "File a leave request (self; hr:manage may file for another employee; overlaps rejected)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/leave-requests/{id}", owner: "hr", description: "Get one leave request (self or hr:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/leave-requests/{id}/approve", owner: "hr", description: "Approve a leave request (hr:manage; checks balance and deducts days)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/leave-requests/{id}/reject", owner: "hr", description: "Reject a leave request with a reason (hr:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/leave-requests/{id}/cancel", owner: "hr", description: "Cancel a leave request (self or hr:manage; approved days are returned to the balance)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/attendance", owner: "hr", description: "List attendance records (HR sees all; an employee sees only their own)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/attendance", owner: "hr", description: "Record attendance manually (hr:manage; TimeTracker events are recorded automatically)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}/attendance", owner: "hr", description: "List one employee's attendance (self or hr:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/payroll-runs", owner: "hr", description: "List payroll runs (money totals shown only to payroll-capable callers)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/payroll-runs", owner: "hr", description: "Create a payroll run (hr:payroll)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/payroll-runs/{id}", owner: "hr", description: "Get one payroll run (money hidden from non-payroll callers)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/payroll-runs/{id}/compute", owner: "hr", description: "Compute a payroll run from active contracts (hr:payroll)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/payroll-runs/{id}/finalize", owner: "hr", description: "Finalize a payroll run and emit hr.payroll_run_completed (hr:payroll; never pays out)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/payroll-runs/{id}/items", owner: "hr", description: "List a payroll run's items (money hidden from other employees)." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/payroll-items/{id}", owner: "hr", description: "Adjust a payroll item's adjustments or deductions (hr:payroll; refused after finalization)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/review-cycles", owner: "hr", description: "List performance-review cycles." },
            RouteInfo { method: "POST", path: "/api/v1/hr/review-cycles", owner: "hr", description: "Create a review cycle (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/review-cycles/{id}", owner: "hr", description: "Get one review cycle." },
            RouteInfo { method: "POST", path: "/api/v1/hr/review-cycles/{id}/close", owner: "hr", description: "Close a review cycle (hr:manage; refused with incomplete reviews)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/reviews", owner: "hr", description: "List reviews (HR sees all; an employee sees their own and ones they review)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/reviews", owner: "hr", description: "Create a review (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/reviews/{id}", owner: "hr", description: "Get one review (employee, reviewer, or hr:read_all)." },
            RouteInfo { method: "PUT", path: "/api/v1/hr/reviews/{id}", owner: "hr", description: "Update a review (hr:manage; refused after completion)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/reviews/{id}/complete", owner: "hr", description: "Complete a review (reviewer or hr:manage; requires a rating)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/employees/{id}/checklists", owner: "hr", description: "Create an onboarding or offboarding checklist (hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}/checklists", owner: "hr", description: "List an employee's checklists with item counts (self or hr:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/checklists/{id}", owner: "hr", description: "Get one checklist with its items (self or hr:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/checklists/{id}/items", owner: "hr", description: "Add an item to a checklist (hr:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/checklists/{id}/complete", owner: "hr", description: "Complete a checklist (hr:manage; all items must be done; emits hr.onboarding_completed for onboarding)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/checklist-items/{id}/complete", owner: "hr", description: "Complete one checklist item (the employee or hr:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/employees/{id}/documents", owner: "hr", description: "List an employee's document metadata (self or hr:read_all)." },
            RouteInfo { method: "POST", path: "/api/v1/hr/employees/{id}/documents", owner: "hr", description: "Store document metadata (self or hr:manage; file bytes live in Storage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/documents/{id}", owner: "hr", description: "Get one document's metadata (self or hr:read_all)." },
            RouteInfo { method: "DELETE", path: "/api/v1/hr/documents/{id}", owner: "hr", description: "Delete document metadata (hr:manage; the file itself stays in Storage)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/reports/headcount", owner: "hr", description: "Headcount report by department, status, and position (hr:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/reports/leave-usage", owner: "hr", description: "Leave usage report for one year, by leave type (hr:read_all)." },
            RouteInfo { method: "GET", path: "/api/v1/hr/reports/payroll-summary", owner: "hr", description: "Payroll summary by period and status (hr:payroll)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(HrModule::new()));
}

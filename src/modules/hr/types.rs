//! Public data types, request/response shapes and their impls.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Capabilities (checked through the `role.check` hook; fail closed).

/// Read HR records (own records for employees without `hr:read_all`).
pub const PERM_HR_READ: &str = "hr:read";
/// Read every employee's HR records, including salary amounts.
pub const PERM_HR_READ_ALL: &str = "hr:read_all";
/// Create/update/delete HR records; approve or reject leave; manage
/// reviews, checklists, and documents. Also grants payroll access.
pub const PERM_HR_MANAGE: &str = "hr:manage";
/// Create, compute, and finalize payroll runs; read payroll amounts.
pub const PERM_HR_PAYROLL: &str = "hr:payroll";

/// Largest money amount accepted in a request, in minor units.
/// NUMERIC(19,4) holds at most 15 integer digits, so anything at or
/// above 1e15 major units (1e17 minor units) is rejected.
pub(crate) const MAX_MONEY_MINOR: i64 = 100_000_000_000_000_000;
/// Largest money amount accepted in a request, in major units.
/// Mirrors `MAX_MONEY_MINOR`: 1e15 major units = 1e17 minor units.
pub(crate) const MAX_MONEY: f64 = 1_000_000_000_000_000.0;
/// Longest free-text field accepted (titles, notes, reasons).
pub(crate) const MAX_TEXT: usize = 2000;
/// Longest single leave request accepted, in inclusive calendar days.
/// A request may not span more than one calendar year of days, so a
/// multi-year pending request can never be filed.
pub(crate) const MAX_LEAVE_REQUEST_DAYS: f64 = 366.0;
/// A work day cannot exceed 24 hours.
pub(crate) const MAX_MINUTES_PER_DAY: u64 = 1440;

// Types

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
    /// Salary band bounds in minor units of the contract currency.
    /// JSON carries major units.
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salary_band_min: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salary_band_max: Option<i64>,
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
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salary_band_min: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salary_band_max: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePosition {
    pub title: Option<String>,
    pub department_id: Option<String>,
    pub level: Option<Option<String>>,
    #[serde(default, deserialize_with = "serde_major_nested_option::deserialize")]
    pub salary_band_min: Option<Option<i64>>,
    #[serde(default, deserialize_with = "serde_major_nested_option::deserialize")]
    pub salary_band_max: Option<Option<i64>>,
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
    /// Monthly pay in minor units of `currency`. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub salary_amount: i64,
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
    #[serde(with = "unified_core::money::serde_major")]
    pub salary_amount: i64,
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContract {
    pub contract_type: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<Option<String>>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub salary_amount: Option<i64>,
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
    pub(crate) fn remaining(&self) -> f64 {
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
/// anyone - finalizing only emits `hr.payroll_run_completed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayrollRun {
    pub id: String,
    pub period: String,
    pub currency: String,
    /// `draft`, `computed`, `finalized`, or `cancelled`.
    pub status: String,
    /// Run totals in minor units of `currency`. JSON carries major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub total_gross: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub total_net: i64,
    pub total_employees: u32,
    /// Employees left out of the last compute, with the reason each
    /// one was skipped (for example a contract in another currency).
    /// Totals cover exactly the employees NOT listed here, and no
    /// payroll item remains stored for a skipped employee.
    #[serde(default)]
    pub skipped_employees: Vec<SkippedPayrollEmployee>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One employee left out of a payroll run compute, with the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedPayrollEmployee {
    pub employee_id: String,
    pub reason: String,
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
    /// Line amounts in minor units of the run currency. JSON carries
    /// major units.
    #[serde(with = "unified_core::money::serde_major")]
    pub gross: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub adjustments: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub deductions: i64,
    #[serde(with = "unified_core::money::serde_major")]
    pub net: i64,
    #[serde(default)]
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePayrollItem {
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub adjustments: Option<i64>,
    #[serde(default, with = "unified_core::money::serde_major::option")]
    pub deductions: Option<i64>,
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

/// One employee document. Metadata only - the file itself lives in
/// Storage under `storage_file_id`; no file bytes are stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmployeeDocument {
    pub id: String,
    pub employee_id: String,
    pub title: String,
    /// `contract`, `id`, `passport`, `certificate`, or `other`.
    pub document_type: String,
    pub storage_file_id: String,
    /// True only when Storage explicitly answered that the file
    /// exists. False means "unknown" (no Storage handler answered)
    /// - the metadata is kept but the file is unverified, never
    /// treated as confirmed to exist.
    #[serde(default)]
    pub storage_verified: bool,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(entitled: f64, carried: f64, used: f64) -> LeaveBalance {
        LeaveBalance {
            id: "bal-1".to_string(),
            employee_id: "emp-1".to_string(),
            leave_type_id: "lt-1".to_string(),
            year: 2026,
            entitled_days: entitled,
            used_days: used,
            carried_over_days: carried,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn remaining_subtracts_used() {
        assert_eq!(balance(12.0, 2.0, 5.0).remaining(), 9.0);
        assert_eq!(balance(10.0, 0.0, 0.0).remaining(), 10.0);
    }

    #[test]
    fn remaining_clamps_at_zero() {
        assert_eq!(balance(5.0, 0.0, 10.0).remaining(), 0.0);
    }
}

/// Serde adapter for `Option<Option<i64>>` money fields (update
/// requests where absent / null / value are three distinct states).
/// JSON carries major units; the inner value converts with the same
/// 2-decimal assumption as `unified_core::money::serde_major`.
pub(crate) mod serde_major_nested_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt = Option::<Option<f64>>::deserialize(deserializer)?;
        Ok(opt.map(|inner| inner.map(|major| (major * 100.0).round() as i64)))
    }
}

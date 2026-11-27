//! In-memory module state.

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
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::*;

// State

pub struct HrState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) employees: DashMap<String, Employee>,
    pub(crate) positions: DashMap<String, Position>,
    pub(crate) contracts: DashMap<String, Contract>,
    pub(crate) leave_types: DashMap<String, LeaveType>,
    pub(crate) leave_balances: DashMap<String, LeaveBalance>,
    pub(crate) leave_requests: DashMap<String, LeaveRequest>,
    pub(crate) attendance: DashMap<String, AttendanceRecord>,
    pub(crate) payroll_runs: DashMap<String, PayrollRun>,
    pub(crate) payroll_items: DashMap<String, PayrollItem>,
    pub(crate) review_cycles: DashMap<String, ReviewCycle>,
    pub(crate) reviews: DashMap<String, Review>,
    pub(crate) checklists: DashMap<String, Checklist>,
    pub(crate) checklist_items: DashMap<String, ChecklistItem>,
    pub(crate) documents: DashMap<String, EmployeeDocument>,
    /// One mutex per employee, serializing leave-balance mutations
    /// (approval charging, cancellation refunds, balance creation)
    /// for that employee. Approvals for one employee must never run
    /// concurrently, or two of them can both pass the balance check
    /// and overdraw the same balance.
    pub(crate) leave_locks: DashMap<String, Arc<Mutex<()>>>,
}

impl HrState {
    pub(crate) fn new() -> Self {
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
            leave_locks: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
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
    pub(crate) fn department_verified(&self, user_id: &str, department_id: &str) -> bool {
        self.department_ids_for_user(user_id)
            .iter()
            .any(|id| id == department_id)
    }

    /// Ask Storage whether `file_id` exists (`storage.file_requested`).
    /// Returns `Some(true)` only when a handler explicitly answers
    /// `{"exists": true}`, `Some(false)` when one answers
    /// `{"exists": false}`, and `None` when no handler answers at
    /// all. `None` is "unknown", never "exists": callers must flag
    /// the document as unverified instead of treating it as present.
    pub(crate) fn storage_file_status(&self, file_id: &str) -> Option<bool> {
        let context = self
            .bus()
            .emit("storage.file_requested", json!({ "file_id": file_id }));
        let mut answered = false;
        for result in &context.results {
            if let Some(exists) = result.get("exists").and_then(|v| v.as_bool()) {
                answered = true;
                if !exists {
                    return Some(false);
                }
            }
        }
        if answered {
            Some(true)
        } else {
            None
        }
    }

    pub(crate) fn employee_by_user(&self, user_id: &str) -> Option<Employee> {
        self.employees
            .iter()
            .find(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.value().clone())
    }

    /// Walk the manager chain above `employee_id`. Returns true when
    /// `candidate_manager_id` would create a cycle (the candidate is
    /// the employee itself or one of its reports).
    pub(crate) fn manager_would_cycle(&self, employee_id: &str, candidate_manager_id: &str) -> bool {
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
    pub(crate) fn leave_overlaps(
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

    /// The mutex serializing leave-balance work for one employee.
    /// Every balance read-check-write for this employee (approval,
    /// cancellation refund, balance creation) must hold it, so two
    /// concurrent approvals cannot both pass the same balance check.
    pub(crate) fn leave_lock(&self, employee_id: &str) -> Arc<Mutex<()>> {
        self.leave_locks
            .entry(employee_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone()
    }

    /// Find or create the balance row for one employee, leave type,
    /// and year. A new row starts from the leave type's default days.
    /// Holds the employee's leave lock for the whole find-or-insert,
    /// so two concurrent calls can never create duplicate rows.
    pub(crate) fn ensure_balance(
        &self,
        employee_id: &str,
        leave_type_id: &str,
        year: i32,
    ) -> UnifiedResult<LeaveBalance> {
        let lock = self.leave_lock(employee_id);
        let _guard = lock
            .lock()
            .map_err(|_| UnifiedError::internal("Leave lock was poisoned"))?;
        self.ensure_balance_locked(employee_id, leave_type_id, year)
    }

    /// `ensure_balance` without taking the lock. Callers must already
    /// hold the employee's leave lock (see `leave_lock`).
    fn ensure_balance_locked(
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

    /// Charge the days of `[start, end]` to the per-year balances,
    /// splitting across years when the range crosses 1 January.
    /// Caller must hold the employee's leave lock. Every affected
    /// year is checked first; only when all years have enough
    /// remaining days is anything deducted, so a short second year
    /// can never leave the first year charged for a failed approval.
    /// Returns the per-year `(year, days)` charges applied.
    pub(crate) fn charge_leave_balances_locked(
        &self,
        employee_id: &str,
        leave_type_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> UnifiedResult<Vec<(i32, f64)>> {
        let charges = leave_days_by_year(start, end);
        let mut balances: Vec<LeaveBalance> = Vec::with_capacity(charges.len());
        for (year, days) in &charges {
            let balance = self.ensure_balance_locked(employee_id, leave_type_id, *year)?;
            if balance.remaining() < *days {
                return Err(UnifiedError::unprocessable(format!(
                    "Insufficient leave balance: {days} day(s) requested in {year}, {} day(s) remaining",
                    balance.remaining()
                )));
            }
            balances.push(balance);
        }
        for (balance, (_, days)) in balances.iter().zip(charges.iter()) {
            let mut stored = balance.clone();
            stored.used_days = round4(stored.used_days + *days);
            stored.updated_at = now();
            self.leave_balances
                .insert(stored.id.clone(), stored);
        }
        Ok(charges)
    }

    /// Return the days of `[start, end]` to the per-year balances,
    /// splitting across years the same way they were charged. Caller
    /// must hold the employee's leave lock. Errors (for example a
    /// missing leave type with no balance row) are returned, never
    /// swallowed: a failed refund must fail the cancellation.
    pub(crate) fn refund_leave_balances_locked(
        &self,
        employee_id: &str,
        leave_type_id: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> UnifiedResult<Vec<(i32, f64)>> {
        let refunds = leave_days_by_year(start, end);
        let mut balances: Vec<LeaveBalance> = Vec::with_capacity(refunds.len());
        for (year, _) in &refunds {
            balances.push(self.ensure_balance_locked(employee_id, leave_type_id, *year)?);
        }
        for (balance, (_, days)) in balances.iter().zip(refunds.iter()) {
            let mut stored = balance.clone();
            stored.used_days = round4((stored.used_days - *days).max(0.0));
            stored.updated_at = now();
            self.leave_balances
                .insert(stored.id.clone(), stored);
        }
        Ok(refunds)
    }

    /// True when a leave type is still referenced by any leave
    /// request or any balance row. Deleting a type both kinds of
    /// rows reference would orphan the balances.
    pub(crate) fn leave_type_in_use(&self, leave_type_id: &str) -> bool {
        self.leave_requests
            .iter()
            .any(|entry| entry.value().leave_type_id == leave_type_id)
            || self
                .leave_balances
                .iter()
                .any(|entry| entry.value().leave_type_id == leave_type_id)
    }

    /// Expire the employee's other active contracts when a new (or
    /// newly activated) contract starts on `new_start`. The previous
    /// contract's term is capped at the day before the new start
    /// date, so its row never claims to be expired while its term
    /// is still in the future: it flips to `expired` only once that
    /// capped end date has passed. A future-dated new contract
    /// therefore leaves the old one in force until the day before
    /// the new one starts, with no coverage gap.
    pub(crate) fn supersede_previous_contracts(
        &self,
        employee_id: &str,
        new_start: NaiveDate,
        today: NaiveDate,
        exclude_id: Option<&str>,
    ) {
        let supersede_end = new_start - chrono::Duration::days(1);
        for mut entry in self.contracts.iter_mut() {
            let contract = entry.value_mut();
            if contract.employee_id != employee_id
                || contract.status != "active"
                || exclude_id == Some(contract.id.as_str())
            {
                continue;
            }
            let capped_end = match contract.end_date {
                Some(end) => Some(end.min(supersede_end)),
                None => Some(supersede_end),
            };
            contract.end_date = capped_end;
            if capped_end.is_some_and(|end| end < today) {
                contract.status = "expired".to_string();
            }
            contract.updated_at = now();
        }
    }

    /// Terminate one employee's live HR records: active contracts
    /// become terminated (ending today, unless they already ended
    /// earlier) and pending or approved leave requests become
    /// cancelled, with approved days refunded to the balances.
    /// Called before the employee status itself is written, so a
    /// failed refund aborts the termination instead of leaving the
    /// employee terminated with live contracts or leave.
    pub(crate) fn terminate_employee_relationships(
        &self,
        employee_id: &str,
        today: NaiveDate,
    ) -> UnifiedResult<()> {
        let lock = self.leave_lock(employee_id);
        let _guard = lock
            .lock()
            .map_err(|_| UnifiedError::internal("Leave lock was poisoned"))?;
        for mut entry in self.contracts.iter_mut() {
            let contract = entry.value_mut();
            if contract.employee_id != employee_id || contract.status != "active" {
                continue;
            }
            contract.status = "terminated".to_string();
            contract.end_date = Some(match contract.end_date {
                Some(end) if end < today => end,
                _ => today,
            });
            contract.updated_at = now();
        }
        let mut to_cancel: Vec<LeaveRequest> = self
            .leave_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                request.employee_id == employee_id
                    && matches!(request.status.as_str(), "pending" | "approved")
            })
            .collect();
        to_cancel.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        for request in to_cancel {
            if request.status == "approved" {
                self.refund_leave_balances_locked(
                    &request.employee_id,
                    &request.leave_type_id,
                    request.start_date,
                    request.end_date,
                )?;
            }
            if let Some(mut stored) = self.leave_requests.get_mut(request.id.as_str()) {
                stored.status = "cancelled".to_string();
                stored.updated_at = now();
            }
        }
        Ok(())
    }

    /// Drop the stored payroll items of `run_id` whose employee is
    /// not in `included_employee_ids` (employees left out of the
    /// last compute). Returns the employee ids whose stale items
    /// were removed, so totals and listed items can never disagree.
    pub(crate) fn remove_stale_payroll_items(
        &self,
        run_id: &str,
        included_employee_ids: &[String],
    ) -> Vec<String> {
        let stale: Vec<String> = self
            .payroll_items
            .iter()
            .filter(|entry| {
                let item = entry.value();
                item.payroll_run_id == run_id
                    && !included_employee_ids.contains(&item.employee_id)
            })
            .map(|entry| entry.key().clone())
            .collect();
        let mut removed: Vec<String> = Vec::with_capacity(stale.len());
        for item_id in stale {
            if let Some((_, item)) = self.payroll_items.remove(item_id.as_str()) {
                removed.push(item.employee_id);
            }
        }
        removed.sort();
        removed.dedup();
        removed
    }

    /// The active contract of one employee, if any: the latest
    /// `active` contract whose start date has passed and whose end
    /// date (if any) has not passed.
    pub(crate) fn active_contract(&self, employee_id: &str, today: NaiveDate) -> Option<Contract> {
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
        contracts.sort_by(|a, b| {
            b.start_date
                .cmp(&a.start_date)
                .then_with(|| a.id.cmp(&b.id))
        });
        contracts.into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn leave_type(id: &str, default_days: f64) -> LeaveType {
        LeaveType {
            id: id.to_string(),
            name: "Annual".to_string(),
            code: "annual".to_string(),
            default_days_per_year: default_days,
            paid: true,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn balance(id: &str, year: i32, entitled: f64, used: f64) -> LeaveBalance {
        LeaveBalance {
            id: id.to_string(),
            employee_id: "emp-1".to_string(),
            leave_type_id: "lt-1".to_string(),
            year,
            entitled_days: entitled,
            used_days: used,
            carried_over_days: 0.0,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn contract(id: &str, start: NaiveDate, end: Option<NaiveDate>, status: &str) -> Contract {
        Contract {
            id: id.to_string(),
            employee_id: "emp-1".to_string(),
            contract_type: "permanent".to_string(),
            start_date: start,
            end_date: end,
            salary_amount: 100000,
            currency: "USD".to_string(),
            status: status.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn leave_request(id: &str, start: NaiveDate, end: NaiveDate, status: &str) -> LeaveRequest {
        LeaveRequest {
            id: id.to_string(),
            employee_id: "emp-1".to_string(),
            leave_type_id: "lt-1".to_string(),
            start_date: start,
            end_date: end,
            days: (end - start).num_days() as f64 + 1.0,
            status: status.to_string(),
            reason: String::new(),
            reviewed_by: None,
            review_note: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn payroll_item(id: &str, run_id: &str, employee_id: &str) -> PayrollItem {
        PayrollItem {
            id: id.to_string(),
            payroll_run_id: run_id.to_string(),
            employee_id: employee_id.to_string(),
            gross: 100000,
            adjustments: 0,
            deductions: 0,
            net: 100000,
            notes: String::new(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn balance_used(state: &HrState, year: i32) -> f64 {
        state
            .leave_balances
            .iter()
            .find(|entry| entry.value().year == year)
            .map(|entry| entry.value().used_days)
            .unwrap_or(-1.0)
    }

    #[test]
    fn ensure_balance_creates_no_duplicate_rows() {
        let state = HrState::new();
        state.leave_types.insert("lt-1".to_string(), leave_type("lt-1", 12.0));
        let first = state.ensure_balance("emp-1", "lt-1", 2026).unwrap();
        let second = state.ensure_balance("emp-1", "lt-1", 2026).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(state.leave_balances.len(), 1);
    }

    #[test]
    fn ensure_balance_creates_one_row_under_concurrent_calls() {
        // Eight threads racing the same find-or-insert must still
        // end up with exactly one balance row for the employee,
        // type, and year.
        let state = Arc::new(HrState::new());
        state
            .leave_types
            .insert("lt-1".to_string(), leave_type("lt-1", 12.0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let shared = state.clone();
            handles.push(std::thread::spawn(move || {
                shared.ensure_balance("emp-1", "lt-1", 2026).unwrap().id
            }));
        }
        let mut ids: Vec<String> = handles
            .into_iter()
            .map(|handle| handle.join().expect("worker thread panicked"))
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 1);
        assert_eq!(state.leave_balances.len(), 1);
    }

    #[test]
    fn leave_lock_is_shared_per_employee() {
        let state = HrState::new();
        let a = state.leave_lock("emp-1");
        let b = state.leave_lock("emp-1");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &state.leave_lock("emp-2")));
    }

    #[test]
    fn charge_splits_days_across_years() {
        let state = HrState::new();
        state.leave_types.insert("lt-1".to_string(), leave_type("lt-1", 10.0));
        let lock = state.leave_lock("emp-1");
        let _guard = lock.lock().unwrap();
        let charges = state
            .charge_leave_balances_locked("emp-1", "lt-1", date(2026, 12, 30), date(2027, 1, 3))
            .unwrap();
        assert_eq!(charges, vec![(2026, 2.0), (2027, 3.0)]);
        assert_eq!(balance_used(&state, 2026), 2.0);
        assert_eq!(balance_used(&state, 2027), 3.0);
    }

    #[test]
    fn charge_is_all_or_nothing_across_years() {
        let state = HrState::new();
        state.leave_types.insert("lt-1".to_string(), leave_type("lt-1", 10.0));
        // Second year can only cover 1 day; the 5-day request must fail
        // without charging anything to the first year.
        state.leave_balances.insert(
            "bal-2027".to_string(),
            balance("bal-2027", 2027, 1.0, 0.0),
        );
        let lock = state.leave_lock("emp-1");
        let _guard = lock.lock().unwrap();
        let result = state.charge_leave_balances_locked(
            "emp-1",
            "lt-1",
            date(2026, 12, 30),
            date(2027, 1, 3),
        );
        assert!(result.is_err());
        assert_eq!(balance_used(&state, 2026), 0.0);
        assert_eq!(balance_used(&state, 2027), 0.0);
    }

    #[test]
    fn refund_returns_days_and_propagates_missing_type_error() {
        let state = HrState::new();
        // No leave type and no balance row: the refund must fail
        // instead of being silently skipped.
        let lock = state.leave_lock("emp-1");
        let _guard = lock.lock().unwrap();
        assert!(state
            .refund_leave_balances_locked("emp-1", "lt-1", date(2026, 6, 1), date(2026, 6, 3))
            .is_err());
        drop(_guard);
        state.leave_types.insert("lt-1".to_string(), leave_type("lt-1", 10.0));
        state.leave_balances.insert(
            "bal-2026".to_string(),
            balance("bal-2026", 2026, 10.0, 3.0),
        );
        let lock = state.leave_lock("emp-1");
        let _guard = lock.lock().unwrap();
        state
            .refund_leave_balances_locked("emp-1", "lt-1", date(2026, 6, 1), date(2026, 6, 3))
            .unwrap();
        assert_eq!(balance_used(&state, 2026), 0.0);
    }

    #[test]
    fn supersede_future_contract_keeps_old_active_with_capped_end() {
        let state = HrState::new();
        state.contracts.insert(
            "c-old".to_string(),
            contract("c-old", date(2026, 1, 1), None, "active"),
        );
        state.supersede_previous_contracts("emp-1", date(2026, 6, 1), date(2026, 3, 1), None);
        let old = state.contracts.get("c-old").unwrap().value().clone();
        assert_eq!(old.status, "active");
        assert_eq!(old.end_date, Some(date(2026, 5, 31)));
    }

    #[test]
    fn supersede_past_contract_expires_old() {
        let state = HrState::new();
        state.contracts.insert(
            "c-old".to_string(),
            contract("c-old", date(2026, 1, 1), None, "active"),
        );
        state.supersede_previous_contracts("emp-1", date(2026, 2, 1), date(2026, 3, 1), None);
        let old = state.contracts.get("c-old").unwrap().value().clone();
        assert_eq!(old.status, "expired");
        assert_eq!(old.end_date, Some(date(2026, 1, 31)));
    }

    #[test]
    fn active_contract_tie_break_is_deterministic() {
        let state = HrState::new();
        state.contracts.insert(
            "c-b".to_string(),
            contract("c-b", date(2026, 1, 1), None, "active"),
        );
        state.contracts.insert(
            "c-a".to_string(),
            contract("c-a", date(2026, 1, 1), None, "active"),
        );
        let picked = state.active_contract("emp-1", date(2026, 6, 1)).unwrap();
        assert_eq!(picked.id, "c-a");
    }

    #[test]
    fn leave_type_in_use_covers_balances() {
        let state = HrState::new();
        assert!(!state.leave_type_in_use("lt-1"));
        state.leave_balances.insert(
            "bal-2026".to_string(),
            balance("bal-2026", 2026, 10.0, 0.0),
        );
        assert!(state.leave_type_in_use("lt-1"));
    }

    #[test]
    fn terminate_employee_relationships_closes_contracts_and_leave() {
        let state = HrState::new();
        state.leave_types.insert("lt-1".to_string(), leave_type("lt-1", 10.0));
        state.contracts.insert(
            "c-1".to_string(),
            contract("c-1", date(2026, 1, 1), None, "active"),
        );
        state.leave_balances.insert(
            "bal-2026".to_string(),
            balance("bal-2026", 2026, 10.0, 3.0),
        );
        state.leave_requests.insert(
            "lr-approved".to_string(),
            leave_request("lr-approved", date(2026, 6, 1), date(2026, 6, 3), "approved"),
        );
        state.leave_requests.insert(
            "lr-pending".to_string(),
            leave_request("lr-pending", date(2026, 7, 1), date(2026, 7, 2), "pending"),
        );
        state.terminate_employee_relationships("emp-1", date(2026, 3, 1)).unwrap();
        let contract = state.contracts.get("c-1").unwrap().value().clone();
        assert_eq!(contract.status, "terminated");
        assert_eq!(contract.end_date, Some(date(2026, 3, 1)));
        assert_eq!(
            state.leave_requests.get("lr-approved").unwrap().value().status,
            "cancelled"
        );
        assert_eq!(
            state.leave_requests.get("lr-pending").unwrap().value().status,
            "cancelled"
        );
        assert_eq!(balance_used(&state, 2026), 0.0);
    }

    #[test]
    fn remove_stale_payroll_items_keeps_included_only() {
        let state = HrState::new();
        state.payroll_items.insert(
            "pi-keep".to_string(),
            payroll_item("pi-keep", "run-1", "emp-1"),
        );
        state.payroll_items.insert(
            "pi-stale".to_string(),
            payroll_item("pi-stale", "run-1", "emp-2"),
        );
        state.payroll_items.insert(
            "pi-other-run".to_string(),
            payroll_item("pi-other-run", "run-2", "emp-2"),
        );
        let removed = state.remove_stale_payroll_items("run-1", &["emp-1".to_string()]);
        assert_eq!(removed, vec!["emp-2".to_string()]);
        assert!(state.payroll_items.contains_key("pi-keep"));
        assert!(!state.payroll_items.contains_key("pi-stale"));
        assert!(state.payroll_items.contains_key("pi-other-run"));
    }

    #[test]
    fn storage_file_status_is_unknown_without_handlers() {
        let state = HrState::new();
        assert_eq!(state.storage_file_status("file-1"), None);
    }
}

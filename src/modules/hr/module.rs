//! Module registration: struct, Module impl and register().

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;
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
// must grant them to the HR roles (see docs/integration/INTEGRATION_NOTES_BATCH3_B2.md).
//
// Events emitted:
//   - `hr.employee_created`        {employee_id, user_id, department_id}
//   - `hr.leave_requested`         {leave_request_id, employee_id, leave_type_id, start_date, end_date, days}
//   - `hr.leave_approved`          {leave_request_id, employee_id, days}
//   - `hr.leave_rejected`          {leave_request_id, employee_id, reason}
//   - `hr.leave_cancelled`         {leave_request_id, employee_id, days}
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
// CONVERGENCE (for the integrator — see docs/integration/INTEGRATION_NOTES_BATCH3_B2.md):
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
//      flagged `storage_verified: false` when nothing answers (the
//      file is "unknown", never treated as existing); a handler that
//      answers `{"exists": false}` makes creation fail with 404.
//   4. The audit module only records events listed in its
//      `AUDITED_EVENTS`; the `hr.*` names above must be added there
//      for HR actions to appear in the audit trail.


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

        ai::register_ai_capabilities("hr", AI_CAPABILITIES);
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
            RouteInfo { method: "DELETE", path: "/api/v1/hr/leave-types/{id}", owner: "hr", description: "Delete a leave type (hr:manage; refused while referenced by leave requests or balances)." },
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
            RouteInfo { method: "PUT", path: "/api/v1/hr/reviews/{id}", owner: "hr", description: "Update a review (hr:manage; refused after completion; completion goes through the complete endpoint)." },
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

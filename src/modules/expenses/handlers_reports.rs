// Handlers: reports.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: reports ------------------------------------------------------

pub(crate) async fn list_reports(
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
        // Tenant scope: a caller-supplied company filter that
        // differs from the resolved tenant is a 403, and rows
        // outside the scope never appear in the list.
        let scope = tenant_scope(&req, company_filter.clone())?;
        let manager = is_manager(&ctx, &caller.role);
        let mut items: Vec<ExpenseReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.company_id.as_deref() != Some(scope.company_id.as_str()) {
                    return false;
                }
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

pub(crate) async fn create_report(
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
        // Tenant scope: the company is stamped from the resolved
        // tenant context, never from the caller's free text. A
        // caller-supplied company_id that differs from the scope
        // is rejected with 403 by `tenant_scope`.
        let scope = tenant_scope(&req, input.company_id.clone())?;
        let company_id = Some(scope.company_id.clone());
        let policy_id = match &input.policy_id {
            Some(raw) => {
                let policy = load_policy(&state, raw)?;
                if !policy.active {
                    return Err(UnifiedError::bad_request(
                        "Policy is not active",
                    ));
                }
                require_company_scope(&policy.company_id, &scope)?;
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

pub(crate) async fn get_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_VIEW)?;
        let scope = tenant_scope(&req, None)?;
        let report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        let lines = report_expenses(&state, report.id);
        Ok(json!({ "report": report, "expenses": lines }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_report(
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
        let scope = tenant_scope(&req, None)?;
        let mut report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
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
                    require_company_scope(&policy.company_id, &scope)?;
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

pub(crate) async fn delete_report(
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
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
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

pub(crate) async fn submit_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let scope = tenant_scope(&req, None)?;
        let report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
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
        // Atomic transition (see `try_submit_transition`): the
        // draft check and the move to `submitted` happen under
        // the per-report write guard, so a second concurrent
        // submit gets a 409 instead of emitting a second
        // approval request that the first one would orphan.
        let (report, request_id) = try_submit_transition(&state, report.id)?;
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

pub(crate) async fn withdraw_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReport> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let scope = tenant_scope(&req, None)?;
        let mut report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        // `apply_withdraw` owns the state move, including
        // clearing the stale decision metadata; the two ids it
        // returns are the outstanding approval requests to
        // cancel on both the expenses-specific and the generic
        // approvals flows.
        let (request_id, generic_request_id) = apply_withdraw(&mut report)?;
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
        if let Some(generic_request_id) = generic_request_id {
            state.emit(
                GENERIC_APPROVAL_CANCELLED,
                json!({
                    "request_id": generic_request_id,
                    "cancelled_by": caller.actor,
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

pub(crate) async fn export_report(
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
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
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

// --- Report transitions (unit-testable state moves) ------------------------

/// Atomically move a draft report to `submitted`. The draft
/// check and the move happen while the per-report DashMap write
/// guard is held, so two concurrent submits cannot both
/// succeed: the second sees `submitted` and gets a 409, and no
/// second approval request is ever emitted or orphaned. The
/// caller runs the line / receipt validation before this call;
/// this function owns only the check-and-set. Returns the
/// updated report and the fresh approval request id.
pub(crate) fn try_submit_transition(
    state: &ExpensesState,
    report_id: Uuid,
) -> UnifiedResult<(ExpenseReport, String)> {
    let Some(mut report) = state.reports.get_mut(&report_id) else {
        return Err(UnifiedError::not_found(format!(
            "Report not found: {report_id}"
        )));
    };
    if report.status != REPORT_DRAFT {
        return Err(UnifiedError::conflict(format!(
            "Only a draft report can be submitted; this report is \"{}\"",
            report.status
        ))
        .with_metadata(
            "allowed_transitions",
            json!(allowed_report_transitions(&report.status)),
        ));
    }
    transition_report(&mut report, REPORT_SUBMITTED)?;
    let request_id = Uuid::now_v7().to_string();
    report.approval_request_id = Some(request_id.clone());
    report.submitted_at = Some(Utc::now());
    report.decided_at = None;
    report.decided_by = None;
    report.decision_reason = None;
    report.updated_at = Utc::now();
    Ok((report.clone(), request_id))
}

/// Withdraw a report: submitted or rejected back to draft. The
/// approval correlation ids are cleared AND the stale decision
/// metadata (`decided_at`, `decided_by`, `decision_reason`) — a
/// withdrawn report is a fresh draft, and the earlier decision
/// must not linger on it. Returns the expenses-side approval
/// request id and the generic approvals request id that were
/// outstanding, so the caller can emit both cancellations.
pub(crate) fn apply_withdraw(
    report: &mut ExpenseReport,
) -> UnifiedResult<(Option<String>, Option<String>)> {
    if report.status != REPORT_SUBMITTED && report.status != REPORT_REJECTED {
        return Err(UnifiedError::conflict(format!(
            "Only a submitted or rejected report can be withdrawn; this report is \"{}\"",
            report.status
        ))
        .with_metadata(
            "allowed_transitions",
            json!(allowed_report_transitions(&report.status)),
        ));
    }
    let request_id = report.approval_request_id.clone();
    let generic_request_id = report.generic_approval_request_id.clone();
    transition_report(report, REPORT_DRAFT)?;
    report.approval_request_id = None;
    report.generic_approval_request_id = None;
    report.submitted_at = None;
    report.decided_at = None;
    report.decided_by = None;
    report.decision_reason = None;
    report.updated_at = Utc::now();
    Ok((request_id, generic_request_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_report() -> ExpenseReport {
        let now = Utc::now();
        ExpenseReport {
            id: Uuid::now_v7(),
            employee_id: "emp-1".to_string(),
            company_id: Some("co-1".to_string()),
            policy_id: None,
            title: "Trip".to_string(),
            notes: None,
            status: REPORT_DRAFT.to_string(),
            currency: "USD".to_string(),
            total_minor: 0,
            violations: vec![],
            approval_request_id: None,
            generic_approval_request_id: None,
            submitted_at: None,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
            reimbursement_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn submit_transition_moves_a_draft_to_submitted_once() {
        let state = ExpensesState::default();
        let report = draft_report();
        let report_id = report.id;
        state.reports.insert(report_id, report);
        let (stored, request_id) = try_submit_transition(&state, report_id).unwrap();
        assert_eq!(stored.status, REPORT_SUBMITTED);
        assert_eq!(stored.approval_request_id.as_deref(), Some(request_id.as_str()));
        assert!(stored.submitted_at.is_some());
        assert!(stored.decided_at.is_none());
        let error = try_submit_transition(&state, report_id).unwrap_err();
        assert_eq!(error.code, 409);
        let stored = state.reports.get(&report_id).unwrap().value().clone();
        assert_eq!(stored.status, REPORT_SUBMITTED);
        assert_eq!(stored.approval_request_id.as_deref(), Some(request_id.as_str()));
    }

    #[test]
    fn concurrent_submits_transition_exactly_once() {
        let state = ExpensesState::default();
        let report = draft_report();
        let report_id = report.id;
        state.reports.insert(report_id, report);
        let outcomes: Vec<bool> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(|| try_submit_transition(&state, report_id).is_ok()));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert_eq!(
            outcomes.iter().filter(|ok| **ok).count(),
            1,
            "exactly one concurrent submit may win; the rest must get a 409"
        );
        let stored = state.reports.get(&report_id).unwrap().value().clone();
        assert_eq!(stored.status, REPORT_SUBMITTED);
        assert!(stored.approval_request_id.is_some());
    }

    #[test]
    fn withdraw_clears_decision_metadata_and_returns_both_request_ids() {
        let mut report = draft_report();
        report.status = REPORT_REJECTED.to_string();
        report.approval_request_id = Some("req-1".to_string());
        report.generic_approval_request_id = Some("generic-1".to_string());
        report.submitted_at = Some(Utc::now());
        report.decided_at = Some(Utc::now());
        report.decided_by = Some("mgr-1".to_string());
        report.decision_reason = Some("Too expensive".to_string());
        let (request_id, generic_request_id) = apply_withdraw(&mut report).unwrap();
        assert_eq!(request_id.as_deref(), Some("req-1"));
        assert_eq!(generic_request_id.as_deref(), Some("generic-1"));
        assert_eq!(report.status, REPORT_DRAFT);
        assert!(report.approval_request_id.is_none());
        assert!(report.generic_approval_request_id.is_none());
        assert!(report.submitted_at.is_none());
        assert!(report.decided_at.is_none());
        assert!(report.decided_by.is_none());
        assert!(report.decision_reason.is_none());
    }

    #[test]
    fn withdraw_rejects_a_report_that_is_not_submitted_or_rejected() {
        let mut report = draft_report();
        let error = apply_withdraw(&mut report).unwrap_err();
        assert_eq!(error.code, 409);
        assert_eq!(report.status, REPORT_DRAFT);
    }
}

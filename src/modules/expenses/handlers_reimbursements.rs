// Handlers: reimbursements.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: reimbursements -----------------------------------------------

pub(crate) async fn list_reimbursements(
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
        let scope = tenant_scope(&req, None)?;
        let mut items: Vec<Reimbursement> = state
            .reimbursements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !manager && row.employee_id != caller.actor {
                    return false;
                }
                // Tenant scope through the parent report: a
                // reimbursement carries no company of its own.
                if !state
                    .reports
                    .get(&row.report_id)
                    .map(|report| {
                        report.company_id.as_deref() == Some(scope.company_id.as_str())
                    })
                    .unwrap_or(false)
                {
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

pub(crate) async fn get_reimbursement(
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
        let report = load_report(&state, &reimbursement.report_id.to_string())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &reimbursement.employee_id)?;
        Ok(reimbursement)
    })();
    match result {
        Ok(reimbursement) => ok(reimbursement),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn retry_reimbursement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Reimbursement> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_MANAGE)?;
        let scope = tenant_scope(&req, None)?;
        let reimbursement = load_reimbursement(&state, &path.into_inner())?;
        let report = load_report(&state, &reimbursement.report_id.to_string())?;
        require_report_scope(&report, &scope)?;
        // Atomic transition (see `try_retry_transition`): the
        // failed check and the move to `pending` happen under
        // the per-reimbursement write guard, so a second
        // concurrent retry gets a 409 instead of re-emitting
        // `expenses.reimbursement.requested`.
        let reimbursement = try_retry_transition(&state, reimbursement.id)?;
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

// --- Reimbursement transitions (unit-testable state moves) ------------------

/// Atomically move a failed reimbursement back to `pending`.
/// The failed check and the move happen while the
/// per-reimbursement DashMap write guard is held, so two
/// concurrent retries cannot both succeed: the second sees
/// `pending` and gets a 409, and the module emits
/// `expenses.reimbursement.requested` exactly once. Returns the
/// updated row.
pub(crate) fn try_retry_transition(
    state: &ExpensesState,
    reimbursement_id: Uuid,
) -> UnifiedResult<Reimbursement> {
    let Some(mut reimbursement) = state.reimbursements.get_mut(&reimbursement_id) else {
        return Err(UnifiedError::not_found(format!(
            "Reimbursement not found: {reimbursement_id}"
        )));
    };
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
    Ok(reimbursement.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_reimbursement() -> Reimbursement {
        let now = Utc::now();
        Reimbursement {
            id: Uuid::now_v7(),
            report_id: Uuid::now_v7(),
            employee_id: "emp-1".to_string(),
            amount_minor: 10_000,
            currency: "USD".to_string(),
            status: REIMB_FAILED.to_string(),
            payable_reference: "expenses-reimbursement-test".to_string(),
            failure_reason: Some("bank refused".to_string()),
            paid_at: None,
            attempts: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn retry_transition_moves_a_failed_reimbursement_to_pending_once() {
        let state = ExpensesState::default();
        let reimbursement = failed_reimbursement();
        let reimbursement_id = reimbursement.id;
        state.reimbursements.insert(reimbursement_id, reimbursement);
        let stored = try_retry_transition(&state, reimbursement_id).unwrap();
        assert_eq!(stored.status, REIMB_PENDING);
        assert_eq!(stored.attempts, 2);
        assert!(stored.failure_reason.is_none());
        let error = try_retry_transition(&state, reimbursement_id).unwrap_err();
        assert_eq!(error.code, 409);
        let stored = state
            .reimbursements
            .get(&reimbursement_id)
            .unwrap()
            .value()
            .clone();
        assert_eq!(stored.attempts, 2);
    }

    #[test]
    fn concurrent_retries_transition_exactly_once() {
        let state = ExpensesState::default();
        let reimbursement = failed_reimbursement();
        let reimbursement_id = reimbursement.id;
        state.reimbursements.insert(reimbursement_id, reimbursement);
        let outcomes: Vec<bool> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(|| try_retry_transition(&state, reimbursement_id).is_ok()));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert_eq!(
            outcomes.iter().filter(|ok| **ok).count(),
            1,
            "exactly one concurrent retry may win; the rest must get a 409"
        );
        let stored = state
            .reimbursements
            .get(&reimbursement_id)
            .unwrap()
            .value()
            .clone();
        assert_eq!(stored.status, REIMB_PENDING);
        assert_eq!(stored.attempts, 2);
    }
}

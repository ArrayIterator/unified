// Handlers: adjustments.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: adjustments ---------------------------------------------------

pub(crate) async fn list_adjustments(
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
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
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

pub(crate) async fn create_adjustment(
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
        let scope = tenant_scope(&req, None)?;
        let report = load_report(&state, &path.into_inner())?;
        require_report_scope(&report, &scope)?;
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
        // Apply the adjustment to the payable: a pending
        // reimbursement is reduced by the adjustment (see
        // `apply_adjustment_to_payable`). The report total and
        // the expense lines are never mutated — the submitted
        // history stays intact.
        let payable_after = apply_adjustment_to_payable(&state, &report, &adjustment);
        state.emit(
            "expenses.adjustment.created",
            json!({
                "adjustment_id": adjustment.id.to_string(),
                "report_id": report.id.to_string(),
                "kind": adjustment.kind,
                "amount_minor": adjustment.amount_minor,
                "currency": adjustment.currency,
                "payable_amount_minor": payable_after,
            }),
        );
        Ok(adjustment)
    })();
    match result {
        Ok(adjustment) => created(adjustment),
        Err(error) => err(error, debug),
    }
}

// --- Adjustment application (unit-testable) ---------------------------------

/// Apply an adjustment to the report's payable: when the report
/// has a reimbursement that is still `pending` and in the same
/// currency, the adjustment reduces its amount (floored at
/// zero) and the new amount is returned. When there is no
/// pending reimbursement — the reimbursement is already paid,
/// the report never produced one, or the currencies differ —
/// there is nothing to reduce and `None` is returned: the
/// adjustment stays record-only and is netted in spend
/// analytics. The report total and the expense lines are never
/// mutated; the submitted history stays intact.
pub(crate) fn apply_adjustment_to_payable(
    state: &ExpensesState,
    report: &ExpenseReport,
    adjustment: &ExpenseAdjustment,
) -> Option<i64> {
    let reimbursement_id = report.reimbursement_id?;
    let mut reimbursement = state.reimbursements.get_mut(&reimbursement_id)?;
    if reimbursement.status != REIMB_PENDING {
        return None;
    }
    if reimbursement.currency != adjustment.currency {
        return None;
    }
    reimbursement.amount_minor = (reimbursement.amount_minor - adjustment.amount_minor).max(0);
    reimbursement.updated_at = Utc::now();
    Some(reimbursement.amount_minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approved_report(reimbursement_id: Option<Uuid>) -> ExpenseReport {
        let now = Utc::now();
        ExpenseReport {
            id: Uuid::now_v7(),
            employee_id: "emp-1".to_string(),
            company_id: Some("co-1".to_string()),
            policy_id: None,
            title: "Trip".to_string(),
            notes: None,
            status: REPORT_APPROVED.to_string(),
            currency: "USD".to_string(),
            total_minor: 10_000,
            violations: vec![],
            approval_request_id: None,
            generic_approval_request_id: None,
            submitted_at: None,
            decided_at: None,
            decided_by: None,
            decision_reason: None,
            reimbursement_id,
            created_at: now,
            updated_at: now,
        }
    }

    fn reimbursement(report_id: Uuid, status: &str, amount: i64, currency: &str) -> Reimbursement {
        let now = Utc::now();
        Reimbursement {
            id: Uuid::now_v7(),
            report_id,
            employee_id: "emp-1".to_string(),
            amount_minor: amount,
            currency: currency.to_string(),
            status: status.to_string(),
            payable_reference: "expenses-reimbursement-test".to_string(),
            failure_reason: None,
            paid_at: None,
            attempts: 1,
            created_at: now,
            updated_at: now,
        }
    }

    fn adjustment(report_id: Uuid, amount: i64, currency: &str) -> ExpenseAdjustment {
        ExpenseAdjustment {
            id: Uuid::now_v7(),
            report_id,
            kind: ADJUST_REFUND.to_string(),
            amount_minor: amount,
            currency: currency.to_string(),
            reason: "Overpaid".to_string(),
            created_by: "mgr-1".to_string(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn refund_reduces_a_pending_reimbursement() {
        let state = ExpensesState::default();
        let report = approved_report(None);
        let reimb = reimbursement(report.id, REIMB_PENDING, 10_000, "USD");
        let reimb_id = reimb.id;
        state.reimbursements.insert(reimb_id, reimb);
        let report = ExpenseReport {
            reimbursement_id: Some(reimb_id),
            ..report
        };
        let adj = adjustment(report.id, 2_500, "USD");
        assert_eq!(
            apply_adjustment_to_payable(&state, &report, &adj),
            Some(7_500)
        );
        assert_eq!(
            state.reimbursements.get(&reimb_id).unwrap().amount_minor,
            7_500
        );
    }

    #[test]
    fn refund_larger_than_the_payable_floors_at_zero() {
        let state = ExpensesState::default();
        let report = approved_report(None);
        let reimb = reimbursement(report.id, REIMB_PENDING, 1_000, "USD");
        let reimb_id = reimb.id;
        state.reimbursements.insert(reimb_id, reimb);
        let report = ExpenseReport {
            reimbursement_id: Some(reimb_id),
            ..report
        };
        let adj = adjustment(report.id, 5_000, "USD");
        assert_eq!(
            apply_adjustment_to_payable(&state, &report, &adj),
            Some(0)
        );
        assert_eq!(
            state.reimbursements.get(&reimb_id).unwrap().amount_minor,
            0
        );
    }

    #[test]
    fn adjustment_is_record_only_once_the_reimbursement_is_paid() {
        let state = ExpensesState::default();
        let report = approved_report(None);
        let reimb = reimbursement(report.id, REIMB_PAID, 10_000, "USD");
        let reimb_id = reimb.id;
        state.reimbursements.insert(reimb_id, reimb);
        let report = ExpenseReport {
            reimbursement_id: Some(reimb_id),
            ..report
        };
        let adj = adjustment(report.id, 2_500, "USD");
        assert_eq!(apply_adjustment_to_payable(&state, &report, &adj), None);
        assert_eq!(
            state.reimbursements.get(&reimb_id).unwrap().amount_minor,
            10_000
        );
    }

    #[test]
    fn adjustment_with_no_reimbursement_or_other_currency_is_record_only() {
        let state = ExpensesState::default();
        let report = approved_report(None);
        let adj = adjustment(report.id, 2_500, "USD");
        assert_eq!(apply_adjustment_to_payable(&state, &report, &adj), None);

        let reimb = reimbursement(report.id, REIMB_PENDING, 10_000, "USD");
        let reimb_id = reimb.id;
        state.reimbursements.insert(reimb_id, reimb);
        let report = ExpenseReport {
            reimbursement_id: Some(reimb_id),
            ..report
        };
        let eur = adjustment(report.id, 2_500, "EUR");
        assert_eq!(apply_adjustment_to_payable(&state, &report, &eur), None);
        assert_eq!(
            state.reimbursements.get(&reimb_id).unwrap().amount_minor,
            10_000
        );
    }
}

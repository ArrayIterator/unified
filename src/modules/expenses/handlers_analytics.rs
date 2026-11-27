// Handlers: analytics.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

// Handlers: analytics

/// Spend totals grouped by `group_by` (`category`, `employee`,
/// `period`) and currency: amounts in different currencies are
/// never summed together, so each row names its currency.
/// Included statuses are `submitted`, `approved`, and
/// `reimbursed` - spend that is in flight or settled. `draft`
/// reports never left the employee, and `rejected` reports were
/// denied, so neither counts as spend. Refunds net against the
/// same (group, currency) row where they can be attributed:
/// employee rows net the employee's refunds, period rows net
/// refunds created in that month; category rows cannot attribute
/// a report-level refund to one category, so they carry no
/// refund netting. `company` scopes the rows to one tenant when
/// given.
pub(crate) fn compute_spend_rows(
    state: &ExpensesState,
    group_by: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    manager: bool,
    actor: &str,
    company: Option<&str>,
) -> Vec<Value> {
    let mut totals: HashMap<(String, String), i64> = HashMap::new();
    let mut counts: HashMap<(String, String), i64> = HashMap::new();
    for entry in state.expenses.iter() {
        let line = entry.value();
        let Some(report) = state.reports.get(&line.report_id) else {
            continue;
        };
        if let Some(company) = company {
            if report.company_id.as_deref() != Some(company) {
                continue;
            }
        }
        if !manager && report.employee_id != actor {
            continue;
        }
        if report.status == REPORT_DRAFT || report.status == REPORT_REJECTED {
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
        let group = match group_by {
            "employee" => report.employee_id.clone(),
            "period" => line.expense_date.format("%Y-%m").to_string(),
            _ => line.category_id.to_string(),
        };
        let key = (group, line.currency.clone());
        *totals.entry(key.clone()).or_insert(0) += line.amount_minor;
        *counts.entry(key).or_insert(0) += 1;
    }
    // Refunds reduce net spend; they are separate entries and
    // never mutate the lines above.
    let mut refund_totals: HashMap<(String, String), i64> = HashMap::new();
    for entry in state.adjustments.iter() {
        let adjustment = entry.value();
        if adjustment.kind != ADJUST_REFUND {
            continue;
        }
        let Some(report) = state.reports.get(&adjustment.report_id) else {
            continue;
        };
        if let Some(company) = company {
            if report.company_id.as_deref() != Some(company) {
                continue;
            }
        }
        if !manager && report.employee_id != actor {
            continue;
        }
        let group = match group_by {
            "employee" => report.employee_id.clone(),
            "period" => adjustment.created_at.format("%Y-%m").to_string(),
            _ => continue,
        };
        let key = (group, adjustment.currency.clone());
        *refund_totals.entry(key).or_insert(0) += adjustment.amount_minor;
    }
    let mut rows: Vec<Value> = totals
        .iter()
        .map(|((group, currency), total)| {
            let key = (group.clone(), currency.clone());
            let refunds = refund_totals.get(&key).copied().unwrap_or(0);
            json!({
                "group": group,
                "group_by": group_by,
                "currency": currency,
                "total_minor": total,
                "expense_count": counts.get(&key).copied().unwrap_or(0),
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
    rows
}

pub(crate) async fn spend_analytics(
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
        let scope = tenant_scope(&req, None)?;
        let rows = compute_spend_rows(
            &state,
            &group_by,
            from,
            to,
            manager,
            &caller.actor,
            Some(&scope.company_id),
        );
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn pending_approvals_analytics(
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
        let scope = tenant_scope(&req, None)?;
        let mut items: Vec<ExpenseReport> = state
            .reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                row.status == REPORT_SUBMITTED
                    && row.company_id.as_deref() == Some(scope.company_id.as_str())
                    && (manager || row.employee_id == caller.actor)
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

pub(crate) async fn reimbursement_backlog_analytics(
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
        let scope = tenant_scope(&req, None)?;
        let mut items: Vec<Reimbursement> = state
            .reimbursements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                (row.status == REIMB_PENDING || row.status == REIMB_FAILED)
                    && (manager || row.employee_id == caller.actor)
                    && state
                        .reports
                        .get(&row.report_id)
                        .map(|report| {
                            report.company_id.as_deref() == Some(scope.company_id.as_str())
                        })
                        .unwrap_or(false)
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

pub(crate) async fn violations_analytics(
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
        let scope = tenant_scope(&req, None)?;
        let mut items: Vec<Value> = Vec::new();
        for entry in state.expenses.iter() {
            let line = entry.value();
            if line.violations.is_empty() {
                continue;
            }
            let Some(report) = state.reports.get(&line.report_id) else {
                continue;
            };
            if report.company_id.as_deref() != Some(scope.company_id.as_str()) {
                continue;
            }
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
            if report.company_id.as_deref() != Some(scope.company_id.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn report(state: &ExpensesState, status: &str, currency: &str, company: &str) -> Uuid {
        let now = Utc::now();
        let id = Uuid::now_v7();
        state.reports.insert(
            id,
            ExpenseReport {
                id,
                employee_id: "emp-1".to_string(),
                company_id: Some(company.to_string()),
                policy_id: None,
                title: "Trip".to_string(),
                notes: None,
                status: status.to_string(),
                currency: currency.to_string(),
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
            },
        );
        id
    }

    fn line(
        state: &ExpensesState,
        report_id: Uuid,
        category_id: Uuid,
        amount: i64,
        currency: &str,
    ) {
        let now = Utc::now();
        let id = Uuid::now_v7();
        state.expenses.insert(
            id,
            Expense {
                id,
                report_id,
                kind: "standard".to_string(),
                category_id,
                expense_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
                amount_minor: amount,
                currency: currency.to_string(),
                description: "Taxi".to_string(),
                project_id: None,
                client_id: None,
                distance: None,
                days: None,
                rate_minor_applied: None,
                violations: vec![],
                created_at: now,
                updated_at: now,
            },
        );
    }

    fn refund(state: &ExpensesState, report_id: Uuid, amount: i64, currency: &str) {
        let id = Uuid::now_v7();
        state.adjustments.insert(
            id,
            ExpenseAdjustment {
                id,
                report_id,
                kind: ADJUST_REFUND.to_string(),
                amount_minor: amount,
                currency: currency.to_string(),
                reason: "Overpaid".to_string(),
                created_by: "mgr-1".to_string(),
                created_at: Utc.with_ymd_and_hms(2026, 11, 21, 10, 0, 0).unwrap(),
            },
        );
    }

    fn row<'a>(rows: &'a [Value], group: &str, currency: &str) -> &'a Value {
        rows.iter()
            .find(|row| {
                row.get("group").and_then(|value| value.as_str()) == Some(group)
                    && row.get("currency").and_then(|value| value.as_str()) == Some(currency)
            })
            .unwrap_or_else(|| panic!("no row for group {group} currency {currency}"))
    }

    #[test]
    fn spend_groups_by_currency_and_excludes_draft_and_rejected() {
        let state = ExpensesState::default();
        let category = Uuid::now_v7();
        let draft = report(&state, REPORT_DRAFT, "USD", "co-1");
        line(&state, draft, category, 100_000, "USD");
        let submitted = report(&state, REPORT_SUBMITTED, "USD", "co-1");
        line(&state, submitted, category, 10_000, "USD");
        let approved = report(&state, REPORT_APPROVED, "EUR", "co-1");
        line(&state, approved, category, 20_000, "EUR");
        let rejected = report(&state, REPORT_REJECTED, "USD", "co-1");
        line(&state, rejected, category, 999_000, "USD");
        let reimbursed = report(&state, REPORT_REIMBURSED, "USD", "co-1");
        line(&state, reimbursed, category, 5_000, "USD");

        let rows = compute_spend_rows(&state, "category", None, None, true, "mgr-1", None);
        assert_eq!(rows.len(), 2, "USD and EUR must never be summed into one row");
        let usd = row(&rows, &category.to_string(), "USD");
        assert_eq!(usd.get("total_minor").and_then(|value| value.as_i64()), Some(15_000));
        assert_eq!(usd.get("expense_count").and_then(|value| value.as_i64()), Some(2));
        let eur = row(&rows, &category.to_string(), "EUR");
        assert_eq!(eur.get("total_minor").and_then(|value| value.as_i64()), Some(20_000));
    }

    #[test]
    fn spend_nets_refunds_per_employee_and_currency() {
        let state = ExpensesState::default();
        let category = Uuid::now_v7();
        let approved = report(&state, REPORT_APPROVED, "USD", "co-1");
        line(&state, approved, category, 10_000, "USD");
        refund(&state, approved, 2_500, "USD");
        refund(&state, approved, 1_000, "EUR");

        let rows = compute_spend_rows(&state, "employee", None, None, true, "mgr-1", None);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get("total_minor").and_then(|value| value.as_i64()), Some(10_000));
        assert_eq!(row.get("refund_minor").and_then(|value| value.as_i64()), Some(2_500));
        assert_eq!(row.get("net_minor").and_then(|value| value.as_i64()), Some(7_500));
    }

    #[test]
    fn spend_nets_refunds_per_period_and_ignores_other_companies() {
        let state = ExpensesState::default();
        let category = Uuid::now_v7();
        let approved = report(&state, REPORT_APPROVED, "USD", "co-1");
        line(&state, approved, category, 10_000, "USD");
        refund(&state, approved, 2_500, "USD");
        let other = report(&state, REPORT_APPROVED, "USD", "co-2");
        line(&state, other, category, 50_000, "USD");

        let rows = compute_spend_rows(
            &state,
            "period",
            None,
            None,
            true,
            "mgr-1",
            Some("co-1"),
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.get("group").and_then(|value| value.as_str()), Some("2026-11"));
        assert_eq!(row.get("total_minor").and_then(|value| value.as_i64()), Some(10_000));
        assert_eq!(row.get("net_minor").and_then(|value| value.as_i64()), Some(7_500));
    }
}

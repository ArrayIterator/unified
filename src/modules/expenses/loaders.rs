// Loaders and report refresh helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use uuid::Uuid;

use super::*;

// --- Loaders ---------------------------------------------------------------

pub(crate) fn load_category(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpenseCategory> {
    let id = parse_uuid(raw_id, "category")?;
    state
        .categories
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category not found: {id}")))
}

pub(crate) fn load_policy(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpensePolicy> {
    let id = parse_uuid(raw_id, "policy")?;
    state
        .policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Policy not found: {id}")))
}

pub(crate) fn load_report(state: &ExpensesState, raw_id: &str) -> UnifiedResult<ExpenseReport> {
    let id = parse_uuid(raw_id, "report")?;
    state
        .reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Report not found: {id}")))
}

pub(crate) fn load_expense(state: &ExpensesState, raw_id: &str) -> UnifiedResult<Expense> {
    let id = parse_uuid(raw_id, "expense")?;
    state
        .expenses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Expense not found: {id}")))
}

pub(crate) fn load_reimbursement(state: &ExpensesState, raw_id: &str) -> UnifiedResult<Reimbursement> {
    let id = parse_uuid(raw_id, "reimbursement")?;
    state
        .reimbursements
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Reimbursement not found: {id}")))
}

pub(crate) fn load_currency_rate(state: &ExpensesState, raw_id: &str) -> UnifiedResult<CurrencyRate> {
    let id = parse_uuid(raw_id, "currency rate")?;
    state
        .currency_rates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Currency rate not found: {id}")))
}

pub(crate) fn report_expenses(state: &ExpensesState, report_id: Uuid) -> Vec<Expense> {
    let mut items: Vec<Expense> = state
        .expenses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|row| row.report_id == report_id)
        .collect();
    items.sort_by(|a, b| (a.expense_date, a.created_at).cmp(&(b.expense_date, b.created_at)));
    items
}

pub(crate) fn expense_receipts(state: &ExpensesState, expense_id: Uuid) -> Vec<ExpenseReceipt> {
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
pub(crate) fn refresh_report_total(state: &ExpensesState, report_id: Uuid) {
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
pub(crate) fn refresh_report_violations(state: &ExpensesState, report_id: Uuid) {
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

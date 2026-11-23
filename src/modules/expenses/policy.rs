// Policy rates and violation checks.

use chrono::Utc;

use super::*;

// --- Policy rates and violation checks -------------------------------------

/// Mileage rate for a report: the policy rate when the report has
/// a policy with one, otherwise the settings default. The rate is
/// policy data, never a constant in a handler.
pub(crate) fn mileage_rate_for(state: &ExpensesState, report: &ExpenseReport) -> i64 {
    let settings = state.settings();
    report
        .policy_id
        .and_then(|policy_id| state.policies.get(&policy_id))
        .and_then(|policy| policy.mileage_rate_minor)
        .unwrap_or(settings.default_mileage_rate_minor)
}

/// Per-diem rate for a report: the policy rate when the report has
/// a policy with one, otherwise the settings default.
pub(crate) fn per_diem_rate_for(state: &ExpensesState, report: &ExpenseReport) -> i64 {
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
pub(crate) fn approval_chain_for(state: &ExpensesState, report: &ExpenseReport) -> Option<String> {
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
pub(crate) fn compute_violations(state: &ExpensesState, report: &ExpenseReport, expense: &Expense) -> Vec<String> {
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
            // Only a VERIFIED receipt clears the flag. A receipt
            // row on its own is an unverified claim: the caller
            // supplies the storage target and key as free text,
            // and no storage lookup runs at attach time (there is
            // no existence-check event on the bus — the storage
            // manager answers `storage.object.upload_requested`
            // with an upload binding, never with "this object
            // exists"). The row becomes verified only when the
            // storage side emits `storage.object.stored` for its
            // key, so a fabricated key can never clear the flag
            // or unblock submission.
            let has_receipt = state
                .receipts
                .iter()
                .any(|entry| entry.value().expense_id == expense.id && entry.value().verified);
            if !has_receipt {
                violations.push(VIOLATION_RECEIPT_MISSING.to_string());
            }
        }
    }
    violations
}

/// Recompute violations for every line of a report and store them.
pub(crate) fn refresh_expense_violations(state: &ExpensesState, report: &ExpenseReport) {
    for line in report_expenses(state, report.id) {
        let violations = compute_violations(state, report, &line);
        if let Some(mut stored) = state.expenses.get_mut(&line.id) {
            stored.violations = violations;
            stored.updated_at = Utc::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn test_state() -> (ExpensesState, Uuid, ExpenseReport, Expense) {
        let state = ExpensesState::default();
        let now = Utc::now();
        let category = ExpenseCategory {
            id: Uuid::now_v7(),
            name: "Travel".to_string(),
            code: "travel".to_string(),
            company_id: Some("co-1".to_string()),
            receipt_required: true,
            receipt_required_above_minor: Some(0),
            per_expense_limit_minor: None,
            per_day_limit_minor: None,
            active: true,
            created_at: now,
            updated_at: now,
        };
        let category_id = category.id;
        state.categories.insert(category_id, category);
        let report = ExpenseReport {
            id: Uuid::now_v7(),
            employee_id: "emp-1".to_string(),
            company_id: Some("co-1".to_string()),
            policy_id: None,
            title: "Trip".to_string(),
            notes: None,
            status: "draft".to_string(),
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
        };
        state.reports.insert(report.id, report.clone());
        let expense = Expense {
            id: Uuid::now_v7(),
            report_id: report.id,
            kind: "standard".to_string(),
            category_id,
            expense_date: chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap(),
            amount_minor: 10_000,
            currency: "USD".to_string(),
            description: "Taxi".to_string(),
            project_id: None,
            client_id: None,
            distance: None,
            days: None,
            rate_minor_applied: None,
            violations: vec![],
            created_at: now,
            updated_at: now,
        };
        state.expenses.insert(expense.id, expense.clone());
        (state, category_id, report, expense)
    }

    fn receipt_for(expense_id: Uuid, verified: bool) -> ExpenseReceipt {
        ExpenseReceipt {
            id: Uuid::now_v7(),
            expense_id,
            storage_target: "expenses".to_string(),
            storage_key: "receipts/taxi.pdf".to_string(),
            file_name: "taxi.pdf".to_string(),
            content_type: Some("application/pdf".to_string()),
            uploaded_by: "emp-1".to_string(),
            verified,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn receipt_missing_flagged_when_no_receipt_exists() {
        let (state, _, report, expense) = test_state();
        let violations = compute_violations(&state, &report, &expense);
        assert!(violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()));
    }

    #[test]
    fn unverified_receipt_keeps_receipt_missing_flag() {
        let (state, _, report, expense) = test_state();
        let receipt = receipt_for(expense.id, false);
        state.receipts.insert(receipt.id, receipt);
        let violations = compute_violations(&state, &report, &expense);
        assert!(
            violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()),
            "a fabricated / unverified receipt reference must not clear the flag"
        );
    }

    #[test]
    fn verified_receipt_clears_receipt_missing_flag() {
        let (state, _, report, expense) = test_state();
        let receipt = receipt_for(expense.id, true);
        state.receipts.insert(receipt.id, receipt);
        let violations = compute_violations(&state, &report, &expense);
        assert!(!violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()));
    }
}

// Hook listeners.

use unified_core::hooks::HookContext;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// Hook listeners

/// Handle an approval decision from the approvals flow. Only a
/// `submitted` report whose stored request id matches the event
/// may move; every other shape is a safe no-op (withdrawn,
/// already decided, or unknown request). Approval creates the
/// reimbursement row and requests the pay-out from billing.
pub(crate) fn on_approval_decided(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    let Some(request_id) = opt_string(&ctx.data, "approval_request_id") else {
        return;
    };
    let Some(report_id_raw) = opt_string(&ctx.data, "report_id") else {
        return;
    };
    let Ok(report_id) = Uuid::parse_str(&report_id_raw) else {
        return;
    };
    let Some(decision) = opt_string(&ctx.data, "decision") else {
        return;
    };
    if decision != REPORT_APPROVED && decision != REPORT_REJECTED {
        return;
    }
    let Some(mut report) = state.reports.get_mut(&report_id) else {
        return;
    };
    if report.status != REPORT_SUBMITTED {
        return;
    }
    if report.approval_request_id.as_deref() != Some(request_id.as_str()) {
        return;
    }
    let decided_by = opt_string(&ctx.data, "decided_by");
    let reason = opt_string(&ctx.data, "reason");
    let now = Utc::now();
    if transition_report(&mut report, &decision).is_err() {
        return;
    }
    report.decided_at = Some(now);
    report.decided_by = decided_by.clone();
    report.decision_reason = reason.clone();
    report.updated_at = now;
    let report_snapshot = report.clone();
    drop(report);
    if decision == REPORT_REJECTED {
        state.emit(
            "expenses.report.rejected",
            json!({
                "report_id": report_id.to_string(),
                "employee_id": report_snapshot.employee_id,
                "decided_by": decided_by,
                "reason": reason,
            }),
        );
        state.notify(
            "expense_report_rejected",
            &report_snapshot.employee_id,
            "Expense report rejected",
            &format!(
                "Your expense report \"{}\" was rejected{}",
                report_snapshot.title,
                reason
                    .as_ref()
                    .map(|value| format!(": {value}"))
                    .unwrap_or_default()
            ),
            None,
        );
        return;
    }
    state.emit(
        "expenses.report.approved",
        json!({
            "report_id": report_id.to_string(),
            "employee_id": report_snapshot.employee_id,
            "total_minor": report_snapshot.total_minor,
            "currency": report_snapshot.currency,
            "decided_by": decided_by,
        }),
    );
    let reimbursement = Reimbursement {
        id: Uuid::now_v7(),
        report_id,
        employee_id: report_snapshot.employee_id.clone(),
        amount_minor: report_snapshot.total_minor,
        currency: report_snapshot.currency.clone(),
        status: REIMB_PENDING.to_string(),
        payable_reference: format!("expenses-reimbursement-{}", Uuid::now_v7()),
        failure_reason: None,
        paid_at: None,
        attempts: 1,
        created_at: now,
        updated_at: now,
    };
    state
        .reimbursements
        .insert(reimbursement.id, reimbursement.clone());
    if let Some(mut stored) = state.reports.get_mut(&report_id) {
        stored.reimbursement_id = Some(reimbursement.id);
        stored.updated_at = Utc::now();
    }
    state.emit(
        "expenses.reimbursement.requested",
        json!({
            "reimbursement_id": reimbursement.id.to_string(),
            "report_id": report_id.to_string(),
            "employee_id": reimbursement.employee_id,
            "amount_minor": reimbursement.amount_minor,
            "currency": reimbursement.currency,
            "payable_reference": reimbursement.payable_reference,
        }),
    );
    state.notify(
        "expense_report_approved",
        &report_snapshot.employee_id,
        "Expense report approved",
        &format!(
            "Your expense report \"{}\" was approved; reimbursement of {} {} has been requested",
            report_snapshot.title, report_snapshot.total_minor, report_snapshot.currency
        ),
        None,
    );
}

/// Emit the generic `approval.requested` for a submitted report and
/// return the approvals `request_id` from the synchronous answer,
/// when the approvals module accepts the request. Returns `None`
/// when nobody answers or the request is declined (no active
/// definition for `expenses` / `expense_report`): the report stays
/// `submitted` and no correlation id is stored.
pub(crate) fn request_generic_approval(state: &ExpensesState, report: &ExpenseReport) -> Option<String> {
    let Some(bus) = state.bus() else {
        return None;
    };
    let summary = format!(
        "Expense report \"{}\" - {} {}",
        report.title, report.total_minor, report.currency
    );
    let context = bus.emit(
        GENERIC_APPROVAL_REQUESTED,
        json!({
            "module": APPROVAL_MODULE,
            "record_type": APPROVAL_RECORD_TYPE,
            "record_id": report.id.to_string(),
            "requester_id": report.employee_id,
            "summary": summary,
            "amount_minor": report.total_minor,
            "currency": report.currency,
            "company_id": report.company_id,
        }),
    );
    let answer = context
        .get("result")
        .cloned()
        .or_else(|| context.results.first().cloned())?;
    if answer.get("accepted").and_then(|value| value.as_bool()) != Some(true) {
        return None;
    }
    answer
        .get("request_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Handle a generic `approval.decided` from the approvals flow.
/// Correlates the decision to a submitted report - by the stored
/// approvals `request_id` when the report carries one, otherwise
/// by the (`module`, `record_type`, `record_id`) triple (requests
/// created before this bridge landed, or by an admin through the
/// approvals API) - then re-emits it as `expenses.approval.decided`
/// so the existing listener applies the outcome (status move,
/// reimbursement, notifications) exactly once. A decision for an
/// unknown request, a mismatched report, or a report that is no
/// longer `submitted` is ignored as a safe no-op. `approved` and
/// `rejected` advance the report; `changes_requested` is re-emitted
/// for listeners but leaves the report `submitted` - the requester
/// withdraws it to draft to revise it.
pub(crate) fn on_generic_approval_decided(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    if opt_string(&ctx.data, "module").as_deref() != Some(APPROVAL_MODULE) {
        return;
    }
    if opt_string(&ctx.data, "record_type").as_deref() != Some(APPROVAL_RECORD_TYPE) {
        return;
    }
    let Some(record_id_raw) = opt_string(&ctx.data, "record_id") else {
        return;
    };
    let Ok(report_id) = Uuid::parse_str(&record_id_raw) else {
        return;
    };
    let Some(decision) = opt_string(&ctx.data, "decision") else {
        return;
    };
    if decision != REPORT_APPROVED
        && decision != REPORT_REJECTED
        && decision != "changes_requested"
    {
        return;
    }
    let Some(report) = state.reports.get(&report_id).map(|entry| entry.value().clone()) else {
        return;
    };
    if report.status != REPORT_SUBMITTED {
        return;
    }
    // When the report carries the approvals request id from submit,
    // the decision must carry the same id - a stale decision for an
    // earlier round (withdrawn and resubmitted) must not apply to
    // the current round. Without a stored id, the record triple
    // above is the correlation.
    if let Some(stored) = &report.generic_approval_request_id {
        if opt_string(&ctx.data, "request_id").as_deref() != Some(stored.as_str()) {
            return;
        }
    }
    // A submitted report always carries its expenses-side request
    // id (set on submit); without it the existing listener could
    // not correlate the re-emitted event, so there is nothing to
    // forward.
    let Some(approval_request_id) = report.approval_request_id.clone() else {
        return;
    };
    let decided_by = opt_string(&ctx.data, "decided_by");
    let reason = opt_string(&ctx.data, "reason");
    state.emit(
        APPROVAL_DECIDED,
        json!({
            "approval_request_id": approval_request_id,
            "report_id": report.id.to_string(),
            "decision": decision,
            "decided_by": decided_by,
            "reason": reason,
        }),
    );
}

/// Shared outcome handler for reimbursement paid / failed events.
/// A paid event moves the reimbursement to `paid` and the report
/// to `reimbursed`; a failed event records the reason and leaves
/// the report `approved` so the reimbursement can be retried.
pub(crate) fn apply_reimbursement_outcome(state: &ExpensesState, ctx: &HookContext, paid: bool) {
    let Some(reimbursement_id_raw) = opt_string(&ctx.data, "reimbursement_id") else {
        return;
    };
    let Ok(reimbursement_id) = Uuid::parse_str(&reimbursement_id_raw) else {
        return;
    };
    let Some(mut reimbursement) = state.reimbursements.get_mut(&reimbursement_id) else {
        return;
    };
    if reimbursement.status == REIMB_PAID {
        return;
    }
    let now = Utc::now();
    if paid {
        reimbursement.status = REIMB_PAID.to_string();
        reimbursement.paid_at = Some(now);
        reimbursement.failure_reason = None;
        reimbursement.updated_at = now;
        let snapshot = reimbursement.clone();
        drop(reimbursement);
        if let Some(mut report) = state.reports.get_mut(&snapshot.report_id) {
            if report.status == REPORT_APPROVED {
                let _ = transition_report(&mut report, REPORT_REIMBURSED);
                report.updated_at = now;
            }
        }
        state.emit(
            "expenses.report.reimbursed",
            json!({
                "report_id": snapshot.report_id.to_string(),
                "reimbursement_id": snapshot.id.to_string(),
                "employee_id": snapshot.employee_id,
                "amount_minor": snapshot.amount_minor,
                "currency": snapshot.currency,
            }),
        );
        state.notify(
            "expense_reimbursed",
            &snapshot.employee_id,
            "Expense reimbursement paid",
            &format!(
                "Your reimbursement of {} {} has been paid",
                snapshot.amount_minor, snapshot.currency
            ),
            None,
        );
    } else {
        reimbursement.status = REIMB_FAILED.to_string();
        reimbursement.failure_reason = opt_string(&ctx.data, "reason");
        reimbursement.updated_at = now;
    }
}

pub(crate) fn on_reimbursement_paid(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    apply_reimbursement_outcome(&state, ctx, true);
}

pub(crate) fn on_reimbursement_failed(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    apply_reimbursement_outcome(&state, ctx, false);
}

/// Generic payment events carry many kinds of payments; only a
/// payload that names a reimbursement id this module owns is a
/// reimbursement outcome. Everything else is ignored.
pub(crate) fn on_payment_event(state: Arc<ExpensesState>, ctx: &mut HookContext, paid: bool) {
    let Some(reimbursement_id_raw) = opt_string(&ctx.data, "reimbursement_id") else {
        return;
    };
    if Uuid::parse_str(&reimbursement_id_raw).is_err() {
        return;
    }
    apply_reimbursement_outcome(&state, ctx, paid);
}

/// Mark every receipt stored under `key` verified (or
/// unverified again when the object was deleted) and recompute
/// the affected expenses' violations, so a receipt only clears
/// the `receipt_missing` flag once the storage side has
/// confirmed the object exists. Matching is by storage key:
/// that is the identity the receipt row stores. When the event
/// names the uploading module, only this module's own uploads
/// verify receipts here - an object another module stored under
/// the same key is not this module's receipt.
fn verify_receipts_for_key(state: &ExpensesState, key: &str, verified: bool) {
    let receipt_ids: Vec<Uuid> = state
        .receipts
        .iter()
        .filter(|entry| entry.value().storage_key == key)
        .map(|entry| *entry.key())
        .collect();
    for receipt_id in receipt_ids {
        let Some(expense_id) = state
            .receipts
            .get(&receipt_id)
            .map(|entry| entry.value().expense_id)
        else {
            continue;
        };
        if let Some(mut receipt) = state.receipts.get_mut(&receipt_id) {
            receipt.verified = verified;
        }
        let Some(expense) = state
            .expenses
            .get(&expense_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        let Some(report) = state
            .reports
            .get(&expense.report_id)
            .map(|entry| entry.value().clone())
        else {
            continue;
        };
        refresh_expense_violations(state, &report);
        state.emit(
            if verified {
                "expenses.receipt.verified"
            } else {
                "expenses.receipt.unverified"
            },
            json!({
                "receipt_id": receipt_id.to_string(),
                "expense_id": expense_id.to_string(),
                "report_id": report.id.to_string(),
                "storage_key": key,
            }),
        );
    }
}

/// Handle a `storage.object.stored` event from the storage
/// manager: the object named by `key` now exists, so receipts
/// referencing that key become verified (see
/// `verify_receipts_for_key`).
pub(crate) fn on_storage_object_stored(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    let Some(key) = opt_string(&ctx.data, "key") else {
        return;
    };
    if let Some(module) = opt_string(&ctx.data, "module") {
        if module != "expenses" {
            return;
        }
    }
    verify_receipts_for_key(&state, &key, true);
}

/// Handle a `storage.object.deleted` event from the storage
/// manager: the object named by `key` is gone, so receipts
/// referencing it stop being verified and the
/// `receipt_missing` flag comes back.
pub(crate) fn on_storage_object_deleted(state: Arc<ExpensesState>, ctx: &mut HookContext) {
    let Some(key) = opt_string(&ctx.data, "key") else {
        return;
    };
    if let Some(module) = opt_string(&ctx.data, "module") {
        if module != "expenses" {
            return;
        }
    }
    verify_receipts_for_key(&state, &key, false);
}

/// Escape one CSV field: quote when it contains a comma, a quote,
/// or a newline, doubling embedded quotes. Formula injection:
/// a field a spreadsheet would read as a formula (leading `=`,
/// `+`, `-`, or `@`, after any leading spaces or tabs) is
/// prefixed with a single quote first, so the exported text
/// stays plain text in the spreadsheet. The prefix is applied
/// before quoting, so a quoted field keeps the quote as its
/// first character.
pub(crate) fn csv_field(value: &str) -> String {
    let first = value
        .trim_start_matches([' ', '\t'])
        .chars()
        .next();
    let sanitized = match first {
        Some('=') | Some('+') | Some('-') | Some('@') => format!("'{value}"),
        _ => value.to_string(),
    };
    if sanitized.contains(',') || sanitized.contains('"') || sanitized.contains('\n') || sanitized.contains('\r') {
        format!("\"{}\"", sanitized.replace('"', "\"\""))
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_field_leaves_plain_values_alone() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field(""), "");
        assert_eq!(csv_field("no comma here"), "no comma here");
    }

    #[test]
    fn csv_field_quotes_and_doubles_embedded_quotes() {
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(csv_field("a\"b,c"), "\"a\"\"b,c\"");
    }

    #[test]
    fn csv_field_prefixes_formula_leads_with_a_quote() {
        assert_eq!(csv_field("=2+2"), "'=2+2");
        assert_eq!(csv_field("+2+2"), "'+2+2");
        assert_eq!(csv_field("-2+2"), "'-2+2");
        assert_eq!(csv_field("@SUM(A1:A2)"), "'@SUM(A1:A2)");
        assert_eq!(csv_field("=cmd|'/C calc'"), "'=cmd|'/C calc'");
    }

    #[test]
    fn csv_field_prefixes_formula_leads_after_whitespace_and_before_quoting() {
        assert_eq!(csv_field("  =2+2"), "'  =2+2");
        assert_eq!(csv_field("\t@x"), "'\t@x");
        assert_eq!(csv_field("=a,b"), "\"'=a,b\"");
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("2+2"), "2+2");
    }

    fn test_state_with_receipt() -> (Arc<ExpensesState>, Uuid) {
        let state = Arc::new(ExpensesState::default());
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
            violations: vec![VIOLATION_RECEIPT_MISSING.to_string()],
            created_at: now,
            updated_at: now,
        };
        state.expenses.insert(expense.id, expense.clone());
        let receipt = ExpenseReceipt {
            id: Uuid::now_v7(),
            expense_id: expense.id,
            storage_target: "expenses".to_string(),
            storage_key: "receipts/taxi.pdf".to_string(),
            file_name: "taxi.pdf".to_string(),
            content_type: Some("application/pdf".to_string()),
            uploaded_by: "emp-1".to_string(),
            verified: false,
            created_at: now,
        };
        let receipt_id = receipt.id;
        state.receipts.insert(receipt_id, receipt);
        (state, receipt_id)
    }

    #[test]
    fn stored_event_verifies_matching_receipt_and_clears_flag() {
        let (state, receipt_id) = test_state_with_receipt();
        let mut ctx = HookContext::new(
            STORAGE_OBJECT_STORED,
            json!({
                "key": "receipts/taxi.pdf",
                "module": "expenses",
                "bucket_id": Uuid::now_v7().to_string(),
            }),
        );
        on_storage_object_stored(state.clone(), &mut ctx);
        let receipt = state.receipts.get(&receipt_id).unwrap().value().clone();
        assert!(receipt.verified);
        let expense = state.expenses.iter().next().unwrap().value().clone();
        assert!(!expense.violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()));
    }

    #[test]
    fn stored_event_for_another_key_or_module_leaves_receipt_unverified() {
        let (state, receipt_id) = test_state_with_receipt();
        let mut wrong_key = HookContext::new(
            STORAGE_OBJECT_STORED,
            json!({ "key": "receipts/other.pdf", "module": "expenses" }),
        );
        on_storage_object_stored(state.clone(), &mut wrong_key);
        let mut wrong_module = HookContext::new(
            STORAGE_OBJECT_STORED,
            json!({ "key": "receipts/taxi.pdf", "module": "documents" }),
        );
        on_storage_object_stored(state.clone(), &mut wrong_module);
        let receipt = state.receipts.get(&receipt_id).unwrap().value().clone();
        assert!(!receipt.verified, "a fabricated key must stay unverified");
        let expense = state.expenses.iter().next().unwrap().value().clone();
        assert!(expense.violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()));
    }

    #[test]
    fn deleted_event_unverifies_receipt_and_restores_flag() {
        let (state, receipt_id) = test_state_with_receipt();
        let mut stored = HookContext::new(
            STORAGE_OBJECT_STORED,
            json!({ "key": "receipts/taxi.pdf", "module": "expenses" }),
        );
        on_storage_object_stored(state.clone(), &mut stored);
        assert!(state.receipts.get(&receipt_id).unwrap().verified);
        let mut deleted = HookContext::new(
            STORAGE_OBJECT_DELETED,
            json!({ "key": "receipts/taxi.pdf", "module": "expenses" }),
        );
        on_storage_object_deleted(state.clone(), &mut deleted);
        assert!(!state.receipts.get(&receipt_id).unwrap().verified);
        let expense = state.expenses.iter().next().unwrap().value().clone();
        assert!(expense.violations.contains(&VIOLATION_RECEIPT_MISSING.to_string()));
    }
}

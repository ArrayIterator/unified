// Handlers: receipts.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// --- Handlers: receipts ------------------------------------------------------

pub(crate) async fn attach_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachReceiptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExpenseReceipt> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let expense = load_expense(&state, &path.into_inner())?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Receipts can only be attached to a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_SLUG_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_REF_LEN)?;
        let file_name = require_text(&input.file_name, "file_name", MAX_NAME_LEN)?;
        let content_type = optional_text(&input.content_type, "content_type", MAX_SLUG_LEN)?;
        let now = Utc::now();
        // The receipt is stored UNVERIFIED: the storage target
        // and key are caller-supplied free text and no storage
        // lookup runs here, so this row is a claim, not proof
        // the object exists. It becomes verified only when the
        // storage side emits `storage.object.stored` for its
        // key (see the listener in hooks.rs), and only a
        // verified receipt clears the `receipt_missing`
        // violation — a fabricated key can never unblock a
        // submission.
        let receipt = ExpenseReceipt {
            id: Uuid::now_v7(),
            expense_id: expense.id,
            storage_target,
            storage_key,
            file_name,
            content_type,
            uploaded_by: caller.actor.clone(),
            verified: false,
            created_at: now,
        };
        state.receipts.insert(receipt.id, receipt.clone());
        // Recompute the flags: an unverified receipt leaves the
        // receipt-missing flag in place.
        let mut expense = expense;
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense);
        state.emit(
            "expenses.receipt.attached",
            json!({
                "receipt_id": receipt.id.to_string(),
                "expense_id": receipt.expense_id.to_string(),
                "report_id": report.id.to_string(),
                "storage_target": receipt.storage_target,
                "storage_key": receipt.storage_key,
            }),
        );
        Ok(receipt)
    })();
    match result {
        Ok(receipt) => created(receipt),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn remove_receipt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExpensesState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXPENSES_SUBMIT)?;
        let (expense_id_raw, receipt_id_raw) = path.into_inner();
        let expense = load_expense(&state, &expense_id_raw)?;
        let report = load_report(&state, &expense.report_id.to_string())?;
        require_report_scope(&report, &tenant_scope(&req, None)?)?;
        require_row_access(&ctx, &caller, &report.employee_id)?;
        if report.status != REPORT_DRAFT {
            return Err(UnifiedError::conflict(format!(
                "Receipts can only be removed from a draft report; this report is \"{}\"",
                report.status
            ))
            .with_metadata("allowed_transitions", json!(allowed_report_transitions(&report.status))));
        }
        let receipt_id = parse_uuid(&receipt_id_raw, "receipt")?;
        let receipt = state
            .receipts
            .get(&receipt_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Receipt not found: {receipt_id}")))?;
        if receipt.expense_id != expense.id {
            return Err(UnifiedError::not_found(format!(
                "Receipt not found: {receipt_id}"
            )));
        }
        state.receipts.remove(&receipt_id);
        // Removing a receipt can restore the receipt-missing flag.
        let mut expense = expense;
        expense.violations = compute_violations(&state, &report, &expense);
        expense.updated_at = Utc::now();
        state.expenses.insert(expense.id, expense);
        state.emit(
            "expenses.receipt.removed",
            json!({
                "receipt_id": receipt.id.to_string(),
                "expense_id": receipt.expense_id.to_string(),
                "report_id": report.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": true, "receipt_id": receipt.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

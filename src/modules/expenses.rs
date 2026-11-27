// Expenses module - employee expense reports with categories,
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod state;
mod helpers;
mod loaders;
mod policy;
mod currency;
mod hooks;
mod handlers_categories;
mod handlers_policies;
mod handlers_reports;
mod handlers_expenses;
mod handlers_receipts;
mod handlers_reimbursements;
mod handlers_adjustments;
mod handlers_currency;
mod handlers_analytics;
mod handlers_settings;
pub mod module;

pub(crate) use self::constants::{MAX_NAME_LEN, MAX_DESCRIPTION_LEN, MAX_CODE_LEN, MAX_SLUG_LEN,
    MAX_REASON_LEN, MAX_REF_LEN, MAX_CURRENCY_LEN, MAX_AMOUNT_MINOR, MAX_MILEAGE_DISTANCE,
    MAX_PER_DIEM_DAYS, PERM_EXPENSES_VIEW, PERM_EXPENSES_SUBMIT, PERM_EXPENSES_MANAGE, REPORT_DRAFT,
    REPORT_SUBMITTED, REPORT_APPROVED, REPORT_REJECTED, REPORT_REIMBURSED, REIMB_PENDING,
    REIMB_PAID, REIMB_FAILED, ADJUST_REFUND, ADJUST_ADJUSTMENT, VIOLATION_OVER_PER_EXPENSE_LIMIT,
    VIOLATION_OVER_PER_DAY_LIMIT, VIOLATION_RECEIPT_MISSING, APPROVAL_DECIDED,
    GENERIC_APPROVAL_REQUESTED, GENERIC_APPROVAL_DECIDED, GENERIC_APPROVAL_CANCELLED,
    APPROVAL_MODULE, APPROVAL_RECORD_TYPE, REIMBURSEMENT_PAID, REIMBURSEMENT_FAILED,
    PAYMENT_CAPTURED, PAYMENT_FAILED, CALCULATOR_RATE_REQUESTED, STORAGE_OBJECT_STORED,
    STORAGE_OBJECT_DELETED};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{ExpensesSettings, ExpenseCategory, ExpensePolicy, ExpenseReport, Expense,
    ExpenseReceipt, Reimbursement, ExpenseAdjustment, CurrencyRate, CreateCategoryRequest,
    UpdateCategoryRequest, CreatePolicyRequest, UpdatePolicyRequest, CreateReportRequest,
    UpdateReportRequest, CreateExpenseRequest, UpdateExpenseRequest, AttachReceiptRequest,
    CreateAdjustmentRequest, CreateCurrencyRateRequest, ConvertRequest, UpdateSettingsRequest,
    ExportQuery, SpendQuery};
pub use self::state::{ExpensesState};
pub(crate) use self::helpers::{Caller, resolve_tenant, tenant_scope, require_company_scope,
    require_report_scope, caller, check_capability, require_permission,
    is_manager, require_row_access, parse_uuid, require_text, optional_text, require_in_list,
    require_currency, require_amount, optional_amount, parse_date, ok, created, err, paginate,
    opt_string, allowed_report_transitions, transition_report};
pub(crate) use self::loaders::{load_category, load_policy, load_report, load_expense,
    load_reimbursement, load_currency_rate, report_expenses, expense_receipts, refresh_report_total,
    refresh_report_violations};
pub(crate) use self::policy::{mileage_rate_for, per_diem_rate_for, approval_chain_for,
    compute_violations, refresh_expense_violations};
pub(crate) use self::currency::{stored_rate, calculator_rate, resolve_rate, convert_amount,
    upsert_currency_rate};
pub(crate) use self::hooks::{on_approval_decided, request_generic_approval,
    on_generic_approval_decided, apply_reimbursement_outcome, on_reimbursement_paid,
    on_reimbursement_failed, on_payment_event, on_storage_object_stored,
    on_storage_object_deleted, csv_field};
pub(crate) use self::handlers_categories::{list_categories, create_category, get_category,
    update_category, delete_category};
pub(crate) use self::handlers_policies::{list_policies, create_policy, get_policy, update_policy,
    delete_policy};
pub(crate) use self::handlers_reports::{list_reports, create_report, get_report, update_report,
    delete_report, submit_report, withdraw_report, export_report, try_submit_transition,
    apply_withdraw};
pub(crate) use self::handlers_expenses::{list_report_expenses, create_expense, get_expense,
    update_expense, delete_expense};
pub(crate) use self::handlers_receipts::{attach_receipt, remove_receipt};
pub(crate) use self::handlers_reimbursements::{list_reimbursements, get_reimbursement,
    retry_reimbursement, try_retry_transition};
pub(crate) use self::handlers_adjustments::{list_adjustments, create_adjustment,
    apply_adjustment_to_payable};
pub(crate) use self::handlers_currency::{list_currency_rates, create_currency_rate,
    get_currency_rate, delete_currency_rate, convert_currency};
pub(crate) use self::handlers_analytics::{spend_analytics, pending_approvals_analytics,
    reimbursement_backlog_analytics, violations_analytics, compute_spend_rows};
pub(crate) use self::handlers_settings::{get_settings, update_settings};
pub use self::module::{ExpensesModule, register};

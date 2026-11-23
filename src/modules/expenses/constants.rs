// Limits, permissions, statuses and event names.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_CODE_LEN: usize = 64;
pub(crate) const MAX_SLUG_LEN: usize = 200;
pub(crate) const MAX_REASON_LEN: usize = 1000;
pub(crate) const MAX_REF_LEN: usize = 512;
pub(crate) const MAX_CURRENCY_LEN: usize = 3;
/// Maximum amount in minor units accepted for one expense or one
/// report total. This is an abuse guard on the input shape, not a
/// business limit: business limits live in category and policy
/// data and are checked separately as violations.
pub(crate) const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000;
/// Maximum distance (in the policy mileage unit) accepted for one
/// mileage entry, and maximum days accepted for one per-diem entry.
pub(crate) const MAX_MILEAGE_DISTANCE: f64 = 100_000.0;
pub(crate) const MAX_PER_DIEM_DAYS: i64 = 365;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_EXPENSES_VIEW: &str = "expenses.view";
pub(crate) const PERM_EXPENSES_SUBMIT: &str = "expenses.submit";
pub(crate) const PERM_EXPENSES_MANAGE: &str = "expenses.manage";

/// Report lifecycle statuses. The strings are the stored status
/// values; the transition table in `allowed_report_transitions`
/// is the only place that decides which move is legal.
pub(crate) const REPORT_DRAFT: &str = "draft";
pub(crate) const REPORT_SUBMITTED: &str = "submitted";
pub(crate) const REPORT_APPROVED: &str = "approved";
pub(crate) const REPORT_REJECTED: &str = "rejected";
pub(crate) const REPORT_REIMBURSED: &str = "reimbursed";

/// Reimbursement statuses.
pub(crate) const REIMB_PENDING: &str = "pending";
pub(crate) const REIMB_PAID: &str = "paid";
pub(crate) const REIMB_FAILED: &str = "failed";

/// Adjustment kinds.
pub(crate) const ADJUST_REFUND: &str = "refund";
pub(crate) const ADJUST_ADJUSTMENT: &str = "adjustment";

/// Violation flag names recorded on an expense.
pub(crate) const VIOLATION_OVER_PER_EXPENSE_LIMIT: &str = "over_per_expense_limit";
pub(crate) const VIOLATION_OVER_PER_DAY_LIMIT: &str = "over_per_day_limit";
pub(crate) const VIOLATION_RECEIPT_MISSING: &str = "receipt_missing";

/// Event names this module listens to.
pub(crate) const APPROVAL_DECIDED: &str = "expenses.approval.decided";
/// Generic approvals contract (see the module header): the event
/// this module emits on submit and the decision event it listens
/// to. `APPROVAL_MODULE` / `APPROVAL_RECORD_TYPE` are the pair the
/// approvals module keys its definition on.
pub(crate) const GENERIC_APPROVAL_REQUESTED: &str = "approval.requested";
pub(crate) const GENERIC_APPROVAL_DECIDED: &str = "approval.decided";
/// Generic approvals cancellation (see the module header): the
/// event this module emits on withdraw when a generic approval
/// request is outstanding. Name and payload (`request_id`,
/// `cancelled_by`) are the approvals module's own cancellation
/// contract (`approvals/module.rs`, `approvals/constants.rs`:
/// `APPROVAL_CANCELLED`). Note the approvals module emits this
/// event itself when its own cancel endpoint runs; it does not
/// consume it as an inbound event, so this emit is the
/// notification other listeners observe, not a call into the
/// approvals engine.
pub(crate) const GENERIC_APPROVAL_CANCELLED: &str = "approval.cancelled";
pub(crate) const APPROVAL_MODULE: &str = "expenses";
pub(crate) const APPROVAL_RECORD_TYPE: &str = "expense_report";
/// Storage events this module listens to for receipt verification
/// (see the module header): the storage manager emits these when
/// an object is stored or deleted.
pub(crate) const STORAGE_OBJECT_STORED: &str = "storage.object.stored";
pub(crate) const STORAGE_OBJECT_DELETED: &str = "storage.object.deleted";
pub(crate) const REIMBURSEMENT_PAID: &str = "expenses.reimbursement.paid";
pub(crate) const REIMBURSEMENT_FAILED: &str = "expenses.reimbursement.failed";
pub(crate) const PAYMENT_CAPTURED: &str = "payment.captured";
pub(crate) const PAYMENT_FAILED: &str = "payment.failed";
pub(crate) const CALCULATOR_RATE_REQUESTED: &str = "calculator.rate_requested";

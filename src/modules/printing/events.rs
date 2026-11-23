//! printing - events.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use super::{paper::*, thermal::*, profiles::*, jobs::*, state::*, auth::*, hooks::*, handlers_paper::*, handlers_profiles::*, handlers_jobs::*, module::*};



// ---------------------------------------------------------------------------
// Events and permissions
// ---------------------------------------------------------------------------
/// Create a print job. Payload: a `CreatePrintJobRequest` plus
/// `requested_by`. The handler answers by setting `job_id` and
/// `status` on the event payload (and adding the same object as a
/// result); on failure it sets `error` instead.
pub const EVENT_PRINT_JOB_REQUESTED: &str = "printing.job.requested";

/// Queue a fresh copy of an existing job. Payload: `{"job_id",
/// "requested_by"}`. Answers with `job_id` (the new job) and
/// `previous_job_id`.
pub const EVENT_PRINT_JOB_REPRINT_REQUESTED: &str = "printing.job.reprint_requested";

/// List print jobs for one invoice. Payload: `{"invoice_id",
/// "status"?, "company_id"?}`. Answers by setting `jobs` (an array
/// of `PrintJobSummary`) and `total` on the event payload.
pub const EVENT_PRINT_JOBS_LIST_REQUESTED: &str = "printing.jobs.list_requested";

/// Emitted when a job reaches `done`. Payload: the job summary.
pub const EVENT_PRINT_JOB_COMPLETED: &str = "printing.job.completed";

/// Emitted when a job reaches `failed`. Payload: the job summary
/// plus `error`.
pub const EVENT_PRINT_JOB_FAILED: &str = "printing.job.failed";

/// Emitted when a job is cancelled. Payload: the job summary.
/// Cancellation is its own outcome, not a failure: it must not
/// corrupt failure metrics or trigger failure notifications.
pub const EVENT_PRINT_JOB_CANCELLED: &str = "printing.job.cancelled";

/// Emitted by the billing module when a school-fee receipt is
/// issued (`school_fees.receipt.print_requested`). The payload
/// carries the receipt fields (`receipt_number`,
/// `student_user_id`, `fee_type`, `period_label`, `amount_text`,
/// `currency`, `method`, `reference`, `invoice_id`, `issued_by`)
/// but no rendered document, so this module renders it as a
/// thermal receipt (80mm) from those fields. The billing module
/// only documents the event today — no emitter was found when this
/// handler was written — so the handler is forward-compatible: it
/// answers with `job_id` and `status` whenever the event arrives.
pub const EVENT_SCHOOL_FEE_RECEIPT_PRINT_REQUESTED: &str =
    "school_fees.receipt.print_requested";

/// Emitted by `process`: the deployment-side dispatcher owns
/// delivery to hardware from here. Payload: the job summary plus
/// the ESC/POS stream (base64) for thermal jobs.
pub const EVENT_PRINT_JOB_DISPATCH_REQUESTED: &str = "printing.job.dispatch_requested";

/// Paper-size change events, for the audit log.
pub const EVENT_PAPER_SIZE_CREATED: &str = "printing.paper_size.created";

pub const EVENT_PAPER_SIZE_UPDATED: &str = "printing.paper_size.updated";

pub const EVENT_PAPER_SIZE_DELETED: &str = "printing.paper_size.deleted";

/// Printer-profile change events, for the audit log.
pub const EVENT_PRINTER_PROFILE_CREATED: &str = "printing.printer_profile.created";

pub const EVENT_PRINTER_PROFILE_UPDATED: &str = "printing.printer_profile.updated";

pub const EVENT_PRINTER_PROFILE_DELETED: &str = "printing.printer_profile.deleted";


/// Read paper sizes, printer profiles, and print jobs.
pub(crate) const PERM_PRINTING_VIEW: &str = "printing.view";

/// Create, update, and delete paper sizes and printer profiles.
pub(crate) const PERM_PRINTING_MANAGE: &str = "printing.manage";

/// Create, process, fail, cancel, and reprint print jobs.
pub(crate) const PERM_PRINTING_JOB: &str = "printing.job";


/// Output kinds a print job may request. Kept as data (validated
/// against this list), not as an enum, so a deployment can add a
/// kind without a code change only by extending this list — the
/// list itself is the closed vocabulary the job state machine
/// understands.
pub const OUTPUT_KINDS: &[&str] = &["paper", "thermal", "pdf", "email"];

/// Job lifecycle states, in order. A cancelled job is its own
/// state (`cancelled`), not a failure: recording cancels as
/// `failed` corrupted failure metrics and fired failure
/// notifications for jobs the requester deliberately stopped.
pub const PRINT_JOB_STATUSES: &[&str] = &["queued", "printing", "done", "failed", "cancelled"];

/// Connection kinds a printer profile may declare.
pub const PRINTER_CONNECTION_TYPES: &[&str] = &["network", "usb", "serial", "file", "cloud"];

/// Character sets / code pages a printer profile may declare.
/// The values are the names the ESC/POS `ESC t n` command
/// understands; `code_page_value` maps them to command bytes.
pub const SUPPORTED_CODE_PAGES: &[&str] = &[
    "cp437", "cp850", "cp852", "cp858", "cp860", "cp863", "cp865", "cp866", "cp1252",
];


/// ESC/POS code-page table (ESC/POS standard): name to the `n`
/// byte of `ESC t n`. Unknown names fall back to 0 (PC437), the
/// printer default, instead of failing the job.
pub(crate) fn code_page_value(name: &str) -> u8 {
    match name {
        "cp437" => 0,
        "cp850" => 2,
        "cp860" => 3,
        "cp863" => 5,
        "cp865" => 7,
        "cp866" => 17,
        "cp852" => 18,
        "cp858" => 19,
        "cp1252" => 16,
        _ => 0,
    }
}

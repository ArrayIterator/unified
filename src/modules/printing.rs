// Printing module — paper sizes, thermal (ESC/POS) printing,
// printer profiles, and the print-job queue.

// Split into one file per concern under `src/modules/printing/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::printing::Item`.

pub mod events;
pub mod paper;
pub mod thermal;
pub mod profiles;
pub mod jobs;
pub mod state;
mod auth;
mod hooks;
mod handlers_paper;
mod handlers_profiles;
mod handlers_jobs;
pub mod module;

pub use self::events::{EVENT_PAPER_SIZE_CREATED, EVENT_PAPER_SIZE_DELETED, EVENT_PAPER_SIZE_UPDATED, EVENT_PRINTER_PROFILE_CREATED, EVENT_PRINTER_PROFILE_DELETED, EVENT_PRINTER_PROFILE_UPDATED, EVENT_PRINT_JOBS_LIST_REQUESTED, EVENT_PRINT_JOB_CANCELLED, EVENT_PRINT_JOB_COMPLETED, EVENT_PRINT_JOB_DISPATCH_REQUESTED, EVENT_PRINT_JOB_FAILED, EVENT_PRINT_JOB_REPRINT_REQUESTED, EVENT_PRINT_JOB_REQUESTED, EVENT_SCHOOL_FEE_RECEIPT_PRINT_REQUESTED, OUTPUT_KINDS, PRINTER_CONNECTION_TYPES, PRINT_JOB_STATUSES, SUPPORTED_CODE_PAGES};
pub(crate) use self::events::{PERM_PRINTING_JOB, PERM_PRINTING_MANAGE, PERM_PRINTING_VIEW, code_page_value};
pub use self::paper::{CreatePaperSize, PaperSize, STANDARD_PAPER_SIZES, StandardPaperSize, UpdatePaperSize, is_standard_paper_size, standard_paper_size};
pub(crate) use self::paper::{valid_paper_size_code, validate_paper_size_dimensions};
pub use self::thermal::{ThermalDocument, ThermalLine, ThermalPreviewRequest, ThermalRender, escpos_bytes, render_thermal, render_thermal_preview};
pub(crate) use self::thermal::{ESC, GS, align_text, default_preview_width, default_thermal_align, validate_thermal_line, wrap_text};
pub use self::profiles::{CreatePrinterProfile, PrinterProfile, UpdatePrinterProfile};
pub(crate) use self::profiles::{default_code_page, default_connection_type, default_profile_width, default_true, validate_printer_profile};
pub use self::jobs::{CreatePrintJobRequest, PrintJob, PrintJobSummary};
pub(crate) use self::jobs::{default_output_kind, job_summary};
pub use self::state::{PrintingState};
pub(crate) use self::state::{new_id, now};
pub(crate) use self::auth::{Caller, caller, check_capability, matches_search, notify_user, paginate, require_permission, require_same_tenant, resolve_tenant, tenant_visible};
pub(crate) use self::hooks::{on_print_job_reprint_requested, on_print_job_requested, on_print_jobs_list_requested, on_school_fee_receipt_print_requested};
pub(crate) use self::handlers_paper::{create_paper_size, delete_paper_size, get_default_paper_size, get_paper_size, list_paper_sizes, set_default_paper_size, update_paper_size};
pub(crate) use self::handlers_profiles::{create_printer_profile, delete_printer_profile, get_printer_profile, list_printer_profiles, set_default_printer_profile, update_printer_profile};
pub(crate) use self::handlers_jobs::{cancel_print_job, complete_print_job, create_print_job, fail_print_job, get_print_job, get_print_job_escpos, list_print_jobs, load_job_for_transition, process_print_job, reprint_print_job, thermal_preview};
pub use self::module::{PrintingModule, register};

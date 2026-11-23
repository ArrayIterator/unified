//! Human Resource Management module (batch 3).
//!
//! Split into one file per concern under `hr/`; this file only
//! declares the submodules and re-exports the public API. The full
//! module design notes live at the top of `hr/module.rs`.
pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
mod auth;
mod validation;
mod handlers_employees;
mod handlers_positions;
mod handlers_contracts;
mod handlers_leave;
mod handlers_attendance;
mod handlers_payroll;
mod handlers_reviews;
mod handlers_checklists;
mod handlers_documents;
mod handlers_reports;
mod hooks;
pub mod module;

// Re-exports from types.rs.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::PERM_HR_READ;
pub use self::types::PERM_HR_READ_ALL;
pub use self::types::PERM_HR_MANAGE;
pub use self::types::PERM_HR_PAYROLL;
pub(crate) use self::types::MAX_MONEY;
pub(crate) use self::types::MAX_TEXT;
pub(crate) use self::types::MAX_LEAVE_REQUEST_DAYS;
pub(crate) use self::types::MAX_MINUTES_PER_DAY;
pub use self::types::Employee;
pub use self::types::CreateEmployee;
pub use self::types::UpdateEmployee;
pub use self::types::Position;
pub use self::types::CreatePosition;
pub use self::types::UpdatePosition;
pub use self::types::Contract;
pub use self::types::CreateContract;
pub use self::types::UpdateContract;
pub use self::types::LeaveType;
pub use self::types::CreateLeaveType;
pub use self::types::UpdateLeaveType;
pub use self::types::LeaveBalance;
pub use self::types::CreateLeaveBalance;
pub use self::types::UpdateLeaveBalance;
pub use self::types::LeaveRequest;
pub use self::types::CreateLeaveRequest;
pub use self::types::ReviewLeaveRequest;
pub use self::types::AttendanceRecord;
pub use self::types::CreateAttendance;
pub use self::types::PayrollRun;
pub use self::types::SkippedPayrollEmployee;
pub use self::types::CreatePayrollRun;
pub use self::types::PayrollItem;
pub use self::types::UpdatePayrollItem;
pub use self::types::ReviewCycle;
pub use self::types::CreateReviewCycle;
pub use self::types::Review;
pub use self::types::CreateReview;
pub use self::types::UpdateReview;
pub use self::types::Checklist;
pub use self::types::CreateChecklist;
pub use self::types::CreateChecklistItem;
pub use self::types::ChecklistItem;
pub use self::types::EmployeeDocument;
pub use self::types::CreateEmployeeDocument;
pub use self::types::OrgChartNode;

// Re-exports from state.rs.
pub use self::state::HrState;

// Re-exports from helpers.rs.
pub(crate) use self::helpers::new_id;
pub(crate) use self::helpers::now;
pub(crate) use self::helpers::today;
pub(crate) use self::helpers::round4;
pub(crate) use self::helpers::leave_days_by_year;
pub(crate) use self::helpers::paginate;
pub(crate) use self::helpers::matches_search;
pub(crate) use self::helpers::contract_view;
pub(crate) use self::helpers::position_view;
pub(crate) use self::helpers::payroll_item_view;

// Re-exports from auth.rs.
pub(crate) use self::auth::Caller;
pub(crate) use self::auth::caller;
pub(crate) use self::auth::can_payroll;
pub(crate) use self::auth::can_read_employee;
pub(crate) use self::auth::check_capability;
pub(crate) use self::auth::is_hr_capable;
pub(crate) use self::auth::load_employee_for_read;
pub(crate) use self::auth::require_permission;
pub(crate) use self::auth::require_read_employee;
pub(crate) use self::auth::require_tenant;

// Re-exports from validation.rs.
pub(crate) use self::validation::require_text;
pub(crate) use self::validation::optional_text;
pub(crate) use self::validation::parse_date;
pub(crate) use self::validation::parse_money;
pub(crate) use self::validation::parse_days;
pub(crate) use self::validation::valid_currency;
pub(crate) use self::validation::valid_period;
pub(crate) use self::validation::slugify;

// Re-exports from handlers_employees.rs.
pub(crate) use self::handlers_employees::list_employees;
pub(crate) use self::handlers_employees::create_employee;
pub(crate) use self::handlers_employees::get_employee;
pub(crate) use self::handlers_employees::update_employee;
pub(crate) use self::handlers_employees::delete_employee;
pub(crate) use self::handlers_employees::get_org_chart;

// Re-exports from handlers_positions.rs.
pub(crate) use self::handlers_positions::list_positions;
pub(crate) use self::handlers_positions::create_position;
pub(crate) use self::handlers_positions::get_position;
pub(crate) use self::handlers_positions::update_position;
pub(crate) use self::handlers_positions::delete_position;

// Re-exports from handlers_contracts.rs.
pub(crate) use self::handlers_contracts::list_contracts_for_employee;
pub(crate) use self::handlers_contracts::create_contract;
pub(crate) use self::handlers_contracts::get_contract;
pub(crate) use self::handlers_contracts::update_contract;
pub(crate) use self::handlers_contracts::terminate_contract;

// Re-exports from handlers_leave.rs.
pub(crate) use self::handlers_leave::list_leave_types;
pub(crate) use self::handlers_leave::create_leave_type;
pub(crate) use self::handlers_leave::get_leave_type;
pub(crate) use self::handlers_leave::update_leave_type;
pub(crate) use self::handlers_leave::delete_leave_type;
pub(crate) use self::handlers_leave::list_leave_balances;
pub(crate) use self::handlers_leave::create_leave_balance;
pub(crate) use self::handlers_leave::update_leave_balance;
pub(crate) use self::handlers_leave::list_leave_requests;
pub(crate) use self::handlers_leave::create_leave_request;
pub(crate) use self::handlers_leave::get_leave_request;
pub(crate) use self::handlers_leave::approve_leave_request;
pub(crate) use self::handlers_leave::reject_leave_request;
pub(crate) use self::handlers_leave::cancel_leave_request;

// Re-exports from handlers_attendance.rs.
pub(crate) use self::handlers_attendance::list_attendance;
pub(crate) use self::handlers_attendance::create_attendance;
pub(crate) use self::handlers_attendance::list_attendance_for_employee;

// Re-exports from handlers_payroll.rs.
pub(crate) use self::handlers_payroll::list_payroll_runs;
pub(crate) use self::handlers_payroll::create_payroll_run;
pub(crate) use self::handlers_payroll::get_payroll_run;
pub(crate) use self::handlers_payroll::compute_payroll_run;
pub(crate) use self::handlers_payroll::finalize_payroll_run;
pub(crate) use self::handlers_payroll::list_payroll_items;
pub(crate) use self::handlers_payroll::update_payroll_item;

// Re-exports from handlers_reviews.rs.
pub(crate) use self::handlers_reviews::list_review_cycles;
pub(crate) use self::handlers_reviews::create_review_cycle;
pub(crate) use self::handlers_reviews::get_review_cycle;
pub(crate) use self::handlers_reviews::close_review_cycle;
pub(crate) use self::handlers_reviews::list_reviews;
pub(crate) use self::handlers_reviews::create_review;
pub(crate) use self::handlers_reviews::get_review;
pub(crate) use self::handlers_reviews::update_review;
pub(crate) use self::handlers_reviews::complete_review;

// Re-exports from handlers_checklists.rs.
pub(crate) use self::handlers_checklists::create_checklist;
pub(crate) use self::handlers_checklists::list_checklists_for_employee;
pub(crate) use self::handlers_checklists::get_checklist;
pub(crate) use self::handlers_checklists::add_checklist_item;
pub(crate) use self::handlers_checklists::complete_checklist_item;
pub(crate) use self::handlers_checklists::complete_checklist;

// Re-exports from handlers_documents.rs.
pub(crate) use self::handlers_documents::list_documents_for_employee;
pub(crate) use self::handlers_documents::create_document;
pub(crate) use self::handlers_documents::get_document;
pub(crate) use self::handlers_documents::delete_document;

// Re-exports from handlers_reports.rs.
pub(crate) use self::handlers_reports::report_headcount;
pub(crate) use self::handlers_reports::report_leave_usage;
pub(crate) use self::handlers_reports::report_payroll_summary;

// Re-exports from hooks.rs.
pub(crate) use self::hooks::on_user_manager_query;
pub(crate) use self::hooks::on_timetracker_entry;

// Re-exports from module.rs.
pub use self::module::HrModule;
pub use self::module::register;


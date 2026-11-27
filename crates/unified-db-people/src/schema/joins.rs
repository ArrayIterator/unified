// Joinable tables and same-query groups for the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// Joinable tables in this bounded context.

diesel::joinable!(hr_contracts -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_employees (employee_id));
diesel::joinable!(hr_leave_balances -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_leave_requests -> hr_employees (employee_id));
diesel::joinable!(hr_leave_requests -> hr_leave_types (leave_type_id));
diesel::joinable!(hr_attendance_records -> hr_employees (employee_id));
diesel::joinable!(hr_payroll_items -> hr_payroll_runs (payroll_run_id));
diesel::joinable!(hr_payroll_items -> hr_employees (employee_id));
diesel::joinable!(hr_reviews -> hr_review_cycles (cycle_id));
diesel::joinable!(hr_reviews -> hr_employees (employee_id));
diesel::joinable!(hr_checklists -> hr_employees (employee_id));
diesel::joinable!(hr_checklist_items -> hr_checklists (checklist_id));
diesel::joinable!(hr_documents -> hr_employees (employee_id));

diesel::joinable!(supervision_template_items -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspections -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspection_items -> supervision_inspections (inspection_id));
diesel::joinable!(supervision_defects -> supervision_inspections (inspection_id));

diesel::joinable!(okr_objectives -> okr_cycles (cycle_id));
diesel::joinable!(okr_key_results -> okr_objectives (objective_id));
diesel::joinable!(okr_checkins -> okr_key_results (key_result_id));
diesel::joinable!(okr_links -> okr_key_results (key_result_id));

diesel::joinable!(applicants -> jobs (job_id));
diesel::joinable!(applicant_stage_history -> applicants (applicant_id));
diesel::joinable!(applicant_stage_history -> jobs (job_id));
diesel::joinable!(interviews -> applicants (applicant_id));
diesel::joinable!(interviews -> jobs (job_id));
diesel::joinable!(evaluations -> applicants (applicant_id));
diesel::joinable!(evaluations -> jobs (job_id));
diesel::joinable!(evaluations -> interviews (interview_id));
diesel::joinable!(offers -> applicants (applicant_id));
diesel::joinable!(offers -> jobs (job_id));

diesel::joinable!(appointment_staff_services -> appointment_services (service_id));
diesel::joinable!(appointments -> appointment_services (service_id));
diesel::joinable!(appointment_reminders -> appointments (appointment_id));

// Tables allowed to appear in the same query, grouped by join graph.
//
// One `allow_tables_to_appear_in_same_query!` invocation generates an
// `AllowTablesToAppearInSameQuery` impl for EVERY ordered pair of its
// tables, so a single list of all tables costs ~275k impls and makes
// rustc run out of memory. The groups below are the connected
// components of the `joinable!` declarations: any two tables that can
// be joined share a group, and no pair outside a group is joinable,
// so splitting loses no query. Tables with no `joinable!` edge need
// no entry at all. If a future query needs two tables from different
// groups, merge the groups (the compiler reports the missing pair
// as E0277).

// Join group 3 (13 tables, e.g. hr_attendance_records (+12 more)).
diesel::allow_tables_to_appear_in_same_query!(
    hr_attendance_records,
    hr_checklist_items,
    hr_checklists,
    hr_contracts,
    hr_documents,
    hr_employees,
    hr_leave_balances,
    hr_leave_requests,
    hr_leave_types,
    hr_payroll_items,
    hr_payroll_runs,
    hr_review_cycles,
    hr_reviews,
);

// Join group 21 (6 tables, e.g. applicant_stage_history (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    applicant_stage_history,
    applicants,
    evaluations,
    interviews,
    jobs,
    offers,
);

// Join group 32 (5 tables, e.g. okr_checkins (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    okr_checkins,
    okr_cycles,
    okr_key_results,
    okr_links,
    okr_objectives,
);

// Join group 34 (5 tables, e.g. supervision_defects (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    supervision_defects,
    supervision_inspection_items,
    supervision_inspection_templates,
    supervision_inspections,
    supervision_template_items,
);

// Join group 36 (4 tables, e.g. appointment_reminders (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    appointment_reminders,
    appointment_services,
    appointment_staff_services,
    appointments,
);

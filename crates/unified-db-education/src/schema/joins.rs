// Joinable tables and same-query groups for the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// Joinable tables in this bounded context.

diesel::joinable!(course_modules -> courses (course_id));
diesel::joinable!(lessons -> course_modules (module_id));
diesel::joinable!(lessons -> courses (course_id));
diesel::joinable!(enrollments -> courses (course_id));
diesel::joinable!(lesson_progress -> enrollments (enrollment_id));
diesel::joinable!(lesson_progress -> lessons (lesson_id));
diesel::joinable!(lesson_progress -> courses (course_id));
diesel::joinable!(quizzes -> lessons (lesson_id));
diesel::joinable!(quizzes -> courses (course_id));
diesel::joinable!(quiz_questions -> quizzes (quiz_id));
diesel::joinable!(quiz_attempts -> quizzes (quiz_id));
diesel::joinable!(quiz_attempts -> lessons (lesson_id));
diesel::joinable!(quiz_attempts -> enrollments (enrollment_id));
diesel::joinable!(reviews -> courses (course_id));
diesel::joinable!(elearning_certificates -> courses (course_id));
diesel::joinable!(learning_path_courses -> learning_paths (path_id));
diesel::joinable!(learning_path_courses -> courses (course_id));

diesel::joinable!(curriculum_levels -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_subjects -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_subjects -> curriculum_levels (level_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_core_competencies -> curriculum_levels (level_id));
diesel::joinable!(curriculum_basic_competencies -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_basic_competencies -> curriculum_core_competencies (core_competency_id));
diesel::joinable!(curriculum_structures -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_structures -> curriculum_levels (level_id));
diesel::joinable!(curriculum_structures -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_grade_descriptors -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_grade_descriptors -> curriculum_assessment_domains (domain_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_core_competencies (core_competency_id));
diesel::joinable!(curriculum_lesson_plans -> curriculum_basic_competencies (basic_competency_id));
diesel::joinable!(curriculum_report_cards -> curriculum_frameworks (framework_id));
diesel::joinable!(curriculum_report_cards -> curriculum_levels (level_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_report_cards (report_card_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_subjects (subject_id));
diesel::joinable!(curriculum_report_card_entries -> curriculum_assessment_domains (domain_id));
diesel::joinable!(curriculum_interventions -> curriculum_basic_competencies (basic_competency_id));

diesel::joinable!(admission_period_quotas -> admission_periods (period_id));
diesel::joinable!(admission_stage_definitions -> admission_periods (period_id));
diesel::joinable!(admission_required_documents -> admission_periods (period_id));
diesel::joinable!(admission_applications -> admission_periods (period_id));
diesel::joinable!(admission_application_scores -> admission_applications (application_id));
diesel::joinable!(admission_application_scores -> admission_periods (period_id));
diesel::joinable!(admission_application_scores -> admission_stage_definitions (stage_id));
diesel::joinable!(admission_checklist_items -> admission_applications (application_id));
diesel::joinable!(admission_checklist_items -> admission_periods (period_id));
diesel::joinable!(admission_checklist_items -> admission_required_documents (required_document_id));
diesel::joinable!(admission_waitlist_entries -> admission_periods (period_id));
diesel::joinable!(admission_waitlist_entries -> admission_applications (application_id));
diesel::joinable!(admission_results -> admission_periods (period_id));
diesel::joinable!(admission_results -> admission_applications (application_id));

diesel::joinable!(academic_students -> academic_programs (program_id));
diesel::joinable!(academic_students -> academic_lecturers (advisor_id));
diesel::joinable!(academic_subjects -> academic_programs (program_id));
diesel::joinable!(academic_calendar_events -> academic_semesters (semester_id));
diesel::joinable!(academic_schedules -> academic_subjects (subject_id));
diesel::joinable!(academic_schedules -> academic_lecturers (lecturer_id));
diesel::joinable!(academic_schedules -> academic_rooms (room_id));
diesel::joinable!(academic_schedules -> academic_semesters (semester_id));
diesel::joinable!(academic_enrollments -> academic_students (student_id));
diesel::joinable!(academic_enrollments -> academic_subjects (subject_id));
diesel::joinable!(academic_enrollments -> academic_semesters (semester_id));
diesel::joinable!(academic_enrollments -> academic_lecturers (advisor_id));
diesel::joinable!(academic_attendance -> academic_schedules (schedule_id));
diesel::joinable!(academic_attendance -> academic_students (student_id));
diesel::joinable!(academic_grade_components -> academic_subjects (subject_id));
diesel::joinable!(academic_grades -> academic_enrollments (enrollment_id));
diesel::joinable!(academic_grades -> academic_grade_components (component_id));
diesel::joinable!(academic_grades -> academic_lecturers (graded_by));
diesel::joinable!(academic_grade_appeals -> academic_enrollments (enrollment_id));
diesel::joinable!(academic_grade_appeals -> academic_grade_components (component_id));
diesel::joinable!(academic_grade_appeals -> academic_students (student_id));
diesel::joinable!(academic_grade_appeals -> academic_lecturers (reviewed_by));
diesel::joinable!(academic_transcripts -> academic_students (student_id));
diesel::joinable!(academic_theses -> academic_students (student_id));
diesel::joinable!(academic_theses -> academic_lecturers (advisor_id));
diesel::joinable!(academic_standing_records -> academic_students (student_id));
diesel::joinable!(academic_standing_records -> academic_semesters (semester_id));

diesel::joinable!(school_fee_schedules -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_charges -> school_fee_schedules (schedule_id));
diesel::joinable!(school_fee_charges -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_discounts -> school_fee_types (fee_type_id));
diesel::joinable!(school_fee_receipts -> school_fee_charges (charge_id));
diesel::joinable!(school_fee_reminders -> school_fee_charges (charge_id));

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

// Join group 1 (57 tables, e.g. ai_company_policies (+56 more)).

// Join group 2 (16 tables, e.g. academic_attendance (+15 more)).
diesel::allow_tables_to_appear_in_same_query!(
    academic_attendance,
    academic_calendar_events,
    academic_enrollments,
    academic_grade_appeals,
    academic_grade_components,
    academic_grades,
    academic_lecturers,
    academic_programs,
    academic_rooms,
    academic_schedules,
    academic_semesters,
    academic_standing_records,
    academic_students,
    academic_subjects,
    academic_theses,
    academic_transcripts,
);

// Join group 5 (12 tables, e.g. curriculum_assessment_domains (+11 more)).
diesel::allow_tables_to_appear_in_same_query!(
    curriculum_assessment_domains,
    curriculum_basic_competencies,
    curriculum_core_competencies,
    curriculum_frameworks,
    curriculum_grade_descriptors,
    curriculum_interventions,
    curriculum_lesson_plans,
    curriculum_levels,
    curriculum_report_card_entries,
    curriculum_report_cards,
    curriculum_structures,
    curriculum_subjects,
);

// Join group 6 (12 tables, e.g. course_modules (+11 more)).
diesel::allow_tables_to_appear_in_same_query!(
    course_modules,
    courses,
    elearning_certificates,
    enrollments,
    learning_path_courses,
    learning_paths,
    lesson_progress,
    lessons,
    quiz_attempts,
    quiz_questions,
    quizzes,
    reviews,
);

// Join group 10 (9 tables, e.g. admission_application_scores (+8 more)).
diesel::allow_tables_to_appear_in_same_query!(
    admission_application_scores,
    admission_applications,
    admission_checklist_items,
    admission_period_quotas,
    admission_periods,
    admission_required_documents,
    admission_results,
    admission_stage_definitions,
    admission_waitlist_entries,
);

// Join group 25 (6 tables, e.g. school_fee_charges (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    school_fee_charges,
    school_fee_discounts,
    school_fee_receipts,
    school_fee_reminders,
    school_fee_schedules,
    school_fee_types,
);

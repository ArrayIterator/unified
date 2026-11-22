-- Roll back 2026-11-22-000093_admissions, in reverse dependency
-- order. Only the tables this migration created are dropped.

DROP INDEX IF EXISTS idx_admission_results_published;
DROP INDEX IF EXISTS idx_admission_results_period_id;
DROP TABLE IF EXISTS admission_results;

DROP INDEX IF EXISTS idx_admission_waitlist_entries_status;
DROP INDEX IF EXISTS idx_admission_waitlist_entries_application_id;
DROP INDEX IF EXISTS idx_admission_waitlist_entries_period_id;
DROP TABLE IF EXISTS admission_waitlist_entries;

DROP INDEX IF EXISTS idx_admission_checklist_items_status;
DROP INDEX IF EXISTS idx_admission_checklist_items_application_id;
DROP TABLE IF EXISTS admission_checklist_items;

DROP INDEX IF EXISTS idx_admission_application_scores_stage_id;
DROP INDEX IF EXISTS idx_admission_application_scores_application_id;
DROP TABLE IF EXISTS admission_application_scores;

DROP INDEX IF EXISTS idx_admission_applications_program;
DROP INDEX IF EXISTS idx_admission_applications_status;
DROP INDEX IF EXISTS idx_admission_applications_applicant;
DROP INDEX IF EXISTS idx_admission_applications_period_id;
DROP TABLE IF EXISTS admission_applications;

DROP INDEX IF EXISTS idx_admission_required_documents_period_id;
DROP TABLE IF EXISTS admission_required_documents;

DROP INDEX IF EXISTS idx_admission_stage_definitions_period_id;
DROP TABLE IF EXISTS admission_stage_definitions;

DROP INDEX IF EXISTS idx_admission_period_quotas_period_id;
DROP TABLE IF EXISTS admission_period_quotas;

DROP INDEX IF EXISTS idx_admission_periods_open_date;
DROP INDEX IF EXISTS idx_admission_periods_status;
DROP TABLE IF EXISTS admission_periods;

DROP TABLE IF EXISTS admission_settings;

-- Roll back 2026-11-22-000091_printing, in reverse dependency
-- order. The billing tables from 2026-11-22-000012_logs_billing
-- and the core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS print_jobs;
DROP TABLE IF EXISTS printer_profiles;
DROP TABLE IF EXISTS invoice_template_versions;
DROP TABLE IF EXISTS invoice_templates;
DROP TABLE IF EXISTS paper_sizes;

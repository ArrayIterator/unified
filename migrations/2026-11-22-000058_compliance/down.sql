-- Roll back 2026-11-22-000058_compliance, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS compliance_breach_timeline;
DROP TABLE IF EXISTS compliance_breach_log;
DROP TABLE IF EXISTS compliance_checklists;
DROP TABLE IF EXISTS compliance_processing_records;
DROP TABLE IF EXISTS compliance_retention_reviews;
DROP TABLE IF EXISTS compliance_retention_policies;
DROP TABLE IF EXISTS compliance_consents;
DROP TABLE IF EXISTS compliance_deletion_items;
DROP TABLE IF EXISTS compliance_deletion_requests;
DROP TABLE IF EXISTS compliance_export_requests;

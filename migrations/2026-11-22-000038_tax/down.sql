-- Roll back 2026-11-22-000038_tax, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS tax_audit;
DROP TABLE IF EXISTS tax_connectors;
DROP TABLE IF EXISTS tax_filings;
DROP TABLE IF EXISTS tax_computations;
DROP TABLE IF EXISTS tax_settings;
DROP TABLE IF EXISTS tax_rules;

-- Roll back 2026-11-22-000048_contracts, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS contract_audit_trail;
DROP TABLE IF EXISTS contract_signers;
DROP TABLE IF EXISTS contracts;
DROP TABLE IF EXISTS contract_templates;

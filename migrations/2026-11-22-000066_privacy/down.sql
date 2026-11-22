-- Roll back 2026-11-22-000066_privacy, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS privacy_audit_entries CASCADE;
DROP TABLE IF EXISTS privacy_notices CASCADE;
DROP TABLE IF EXISTS privacy_erasure_requests CASCADE;
DROP TABLE IF EXISTS privacy_consent_history CASCADE;
DROP TABLE IF EXISTS privacy_consents CASCADE;
DROP TABLE IF EXISTS privacy_visibility_defaults CASCADE;
DROP TABLE IF EXISTS privacy_anonymizations CASCADE;
DROP TABLE IF EXISTS privacy_field_declarations CASCADE;

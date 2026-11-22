-- Roll back 2026-11-22-000068_sharing_auditor, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS auditor_sessions CASCADE;
DROP TABLE IF EXISTS auditor_scopes CASCADE;
DROP TABLE IF EXISTS auditor_accounts CASCADE;
DROP TABLE IF EXISTS public_link_access_log CASCADE;
DROP TABLE IF EXISTS public_links CASCADE;

-- Roll back 2026-11-22-000005_certificates, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS certificate_settings;
DROP TABLE IF EXISTS certificate_renewals;
DROP TABLE IF EXISTS certificates;

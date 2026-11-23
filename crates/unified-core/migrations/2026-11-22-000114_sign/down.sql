-- Down migration for 2026-11-22-000114_sign: drop all sign tables.
DROP TABLE IF EXISTS sign_settings;
DROP TABLE IF EXISTS sign_certificates;
DROP TABLE IF EXISTS sign_audit_entries;
DROP TABLE IF EXISTS sign_templates;
DROP TABLE IF EXISTS sign_signers;
DROP TABLE IF EXISTS sign_envelopes;

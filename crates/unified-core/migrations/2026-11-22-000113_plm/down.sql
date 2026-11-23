-- Down migration for 2026-11-22-000113_plm: drop all PLM tables.
DROP TABLE IF EXISTS plm_settings;
DROP TABLE IF EXISTS plm_audit_entries;
DROP TABLE IF EXISTS plm_bom_versions;
DROP TABLE IF EXISTS plm_approvals;
DROP TABLE IF EXISTS plm_ecos;
DROP TABLE IF EXISTS plm_versions;
DROP TABLE IF EXISTS plm_stages;

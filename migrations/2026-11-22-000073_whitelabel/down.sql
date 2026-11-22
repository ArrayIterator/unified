-- Roll back migration 2026-11-22-000073_whitelabel.
-- Drops the five white labelling tables (history first: it only
-- references the other tables by plain string ids, but dropping it
-- first keeps the rollback order obvious).

DROP TABLE IF EXISTS whitelabel_history;
DROP TABLE IF EXISTS whitelabel_profiles;
DROP TABLE IF EXISTS whitelabel_global_defaults;
DROP TABLE IF EXISTS whitelabel_company_settings;
DROP TABLE IF EXISTS whitelabel_global_settings;

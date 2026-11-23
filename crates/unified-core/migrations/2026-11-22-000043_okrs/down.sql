-- Down migration for 2026-11-22-000043_okrs: drop the OKR tables
-- in reverse dependency order (links and check-ins reference key
-- results, key results reference objectives, objectives reference
-- cycles).

DROP TABLE IF EXISTS okr_links;
DROP TABLE IF EXISTS okr_checkins;
DROP TABLE IF EXISTS okr_key_results;
DROP TABLE IF EXISTS okr_objectives;
DROP TABLE IF EXISTS okr_cycles;

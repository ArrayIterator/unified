-- Down migration for 2026-11-22-000119_esg: drop the ESG
-- tables in reverse dependency order. The tables hold no
-- foreign keys, so the order is by definition (audit and
-- history first, definitions last).

DROP TABLE IF EXISTS esg_audit;
DROP TABLE IF EXISTS esg_targets;
DROP TABLE IF EXISTS esg_entry_history;
DROP TABLE IF EXISTS esg_entries;
DROP TABLE IF EXISTS esg_periods;
DROP TABLE IF EXISTS esg_metrics;
DROP TABLE IF EXISTS esg_frameworks;

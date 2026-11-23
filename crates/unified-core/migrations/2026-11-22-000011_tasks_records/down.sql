-- Roll back 2026-11-22-000011_tasks_records, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS system_records;
DROP TABLE IF EXISTS task_checklist_items;
DROP TABLE IF EXISTS task_comments;
DROP TABLE IF EXISTS tasks;

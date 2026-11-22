-- Roll back 2026-11-22-000035_todos, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.
-- This migration has never been executed against a live PostgreSQL
-- database (see up.sql).

DROP TABLE IF EXISTS todo_reminders;
DROP TABLE IF EXISTS todo_completion_log;
DROP TABLE IF EXISTS todo_shares;
DROP TABLE IF EXISTS todo_subtasks;
DROP TABLE IF EXISTS todos;
DROP TABLE IF EXISTS todo_lists;

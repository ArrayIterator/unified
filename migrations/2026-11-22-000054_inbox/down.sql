-- Roll back 2026-11-22-000054_inbox, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS inbox_thread_reads;
DROP TABLE IF EXISTS inbox_canned_replies;
DROP TABLE IF EXISTS inbox_assignments;
DROP TABLE IF EXISTS inbox_threads;

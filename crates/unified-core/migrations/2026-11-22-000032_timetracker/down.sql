-- Roll back 2026-11-22-000032_timetracker, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core, the AI tables from
-- 2026-11-22-000030_ai, and the project tables from
-- 2026-11-22-000031_projects are untouched.

DROP TABLE IF EXISTS timetracker_buyer_settings CASCADE;
DROP TABLE IF EXISTS timetracker_staff_invites CASCADE;
DROP TABLE IF EXISTS timetracker_staff_settings CASCADE;
DROP TABLE IF EXISTS timetracker_team_members CASCADE;
DROP TABLE IF EXISTS timetracker_teams CASCADE;
DROP TABLE IF EXISTS timetracker_user_settings CASCADE;
DROP TABLE IF EXISTS timetracker_settings CASCADE;
DROP TABLE IF EXISTS timetracker_timesheets CASCADE;
DROP TABLE IF EXISTS timetracker_rates CASCADE;
DROP TABLE IF EXISTS timetracker_screenshots CASCADE;
DROP TABLE IF EXISTS timetracker_app_usage CASCADE;
DROP TABLE IF EXISTS timetracker_activity CASCADE;
DROP TABLE IF EXISTS timetracker_timers CASCADE;
DROP TABLE IF EXISTS timetracker_entries CASCADE;

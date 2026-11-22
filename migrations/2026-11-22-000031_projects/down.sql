-- Roll back 2026-11-22-000031_projects, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core and the AI tables from
-- 2026-11-22-000030_ai are untouched.

DROP TABLE IF EXISTS project_activity CASCADE;
DROP TABLE IF EXISTS project_templates CASCADE;
DROP TABLE IF EXISTS project_automation_rules CASCADE;
DROP TABLE IF EXISTS project_time_entries CASCADE;
DROP TABLE IF EXISTS project_task_attachments CASCADE;
DROP TABLE IF EXISTS project_task_watchers CASCADE;
DROP TABLE IF EXISTS project_task_comments CASCADE;
DROP TABLE IF EXISTS project_task_dependencies CASCADE;
DROP TABLE IF EXISTS project_tasks CASCADE;
DROP TABLE IF EXISTS project_business_cases CASCADE;
DROP TABLE IF EXISTS project_program_increments CASCADE;
DROP TABLE IF EXISTS project_cycles CASCADE;
DROP TABLE IF EXISTS project_phases CASCADE;
DROP TABLE IF EXISTS project_sprints CASCADE;
DROP TABLE IF EXISTS project_milestones CASCADE;
DROP TABLE IF EXISTS project_board_columns CASCADE;
DROP TABLE IF EXISTS project_boards CASCADE;
DROP TABLE IF EXISTS project_members CASCADE;
DROP TABLE IF EXISTS projects CASCADE;

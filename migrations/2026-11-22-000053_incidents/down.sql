-- Roll back migration 2026-11-22-000053_incidents. Drops the
-- tables in reverse dependency order (`incidents` is dropped
-- before the escalation policies and rotations it references).
-- The timeline is append-only in the module, but the rollback
-- still drops it: a rollback is an explicit operator action, not
-- a module delete.
DROP TABLE IF EXISTS auto_open_rules;
DROP TABLE IF EXISTS postmortem_action_items;
DROP TABLE IF EXISTS incident_postmortems;
DROP TABLE IF EXISTS page_attempts;
DROP TABLE IF EXISTS incident_escalations;
DROP TABLE IF EXISTS incident_timeline;
DROP TABLE IF EXISTS incidents;
DROP TABLE IF EXISTS escalation_policies;
DROP TABLE IF EXISTS on_call_schedules;
DROP TABLE IF EXISTS on_call_rotations;

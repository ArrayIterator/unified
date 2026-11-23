-- Phone module (`src/modules/phone/`): drop every table
-- created by `2026-11-22-000116_phone/up.sql`, in reverse
-- dependency order. Like the up migration, this has never
-- been executed against a live PostgreSQL database.

DROP TABLE IF EXISTS phone_settings;
DROP TABLE IF EXISTS phone_recordings;
DROP TABLE IF EXISTS phone_auto_response_scripts;
DROP TABLE IF EXISTS phone_ivr_nodes;
DROP TABLE IF EXISTS phone_ivr_menus;
DROP TABLE IF EXISTS phone_presence;
DROP TABLE IF EXISTS phone_routing_rules;
DROP TABLE IF EXISTS phone_queue_waiters;
DROP TABLE IF EXISTS phone_queue_members;
DROP TABLE IF EXISTS phone_queues;
DROP TABLE IF EXISTS phone_signals;
DROP TABLE IF EXISTS phone_call_participants;
DROP TABLE IF EXISTS phone_calls;

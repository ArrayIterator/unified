-- Roll back 2026-11-22-000034_events, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS event_reminders;
DROP TABLE IF EXISTS event_waitlist;
DROP TABLE IF EXISTS event_attendees;
DROP TABLE IF EXISTS event_registrations;
DROP TABLE IF EXISTS event_ticket_types;
DROP TABLE IF EXISTS event_occurrences;
DROP TABLE IF EXISTS events;

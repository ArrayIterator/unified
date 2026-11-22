-- Roll back 2026-11-22-000081_webinars, in reverse dependency
-- order. The meetings tables from 2026-11-22-000027_meetings and the
-- core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS webinar_recordings;
DROP TABLE IF EXISTS webinar_attendance;
DROP TABLE IF EXISTS webinar_questions;
DROP TABLE IF EXISTS webinar_poll_votes;
DROP TABLE IF EXISTS webinar_polls;
DROP TABLE IF EXISTS webinar_registrations;
DROP TABLE IF EXISTS webinars;

-- Roll back migration 2026-11-22-000077_appointments.
-- Drops the five tables created by up.sql, in reverse dependency
-- order. CASCADE covers the indexes and the foreign keys between
-- them; nothing outside this migration references these tables.

DROP TABLE IF EXISTS appointment_reminders CASCADE;
DROP TABLE IF EXISTS appointments CASCADE;
DROP TABLE IF EXISTS appointment_staff_services CASCADE;
DROP TABLE IF EXISTS appointment_staff_availability CASCADE;
DROP TABLE IF EXISTS appointment_services CASCADE;

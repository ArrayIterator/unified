-- Down migration for 2026-11-22-000094_academic: drop the
-- Academic System tables in reverse dependency order.
DROP TABLE IF EXISTS academic_settings;
DROP TABLE IF EXISTS academic_definitions;
DROP TABLE IF EXISTS academic_standing_records;
DROP TABLE IF EXISTS academic_theses;
DROP TABLE IF EXISTS academic_transcripts;
DROP TABLE IF EXISTS academic_grade_appeals;
DROP TABLE IF EXISTS academic_grades;
DROP TABLE IF EXISTS academic_grade_components;
DROP TABLE IF EXISTS academic_attendance;
DROP TABLE IF EXISTS academic_enrollments;
DROP TABLE IF EXISTS academic_schedules;
DROP TABLE IF EXISTS academic_calendar_events;
DROP TABLE IF EXISTS academic_semesters;
DROP TABLE IF EXISTS academic_rooms;
DROP TABLE IF EXISTS academic_subjects;
DROP TABLE IF EXISTS academic_lecturers;
DROP TABLE IF EXISTS academic_students;
DROP TABLE IF EXISTS academic_programs;

-- Roll back 2026-11-22-000055_surveys, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS survey_answers;
DROP TABLE IF EXISTS survey_responses;
DROP TABLE IF EXISTS survey_distributions;
DROP TABLE IF EXISTS survey_questions;
DROP TABLE IF EXISTS surveys;

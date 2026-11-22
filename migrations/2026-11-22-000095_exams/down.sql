-- Down migration for 2026-11-22-000095_exams: drop the Online
-- Exams tables in reverse dependency order.
DROP TABLE IF EXISTS exam_results;
DROP TABLE IF EXISTS exam_proctor_signals;
DROP TABLE IF EXISTS exam_grades;
DROP TABLE IF EXISTS exam_attempts;
DROP TABLE IF EXISTS exam_rubrics;
DROP TABLE IF EXISTS exam_question_links;
DROP TABLE IF EXISTS exams;
DROP TABLE IF EXISTS exam_question_versions;
DROP TABLE IF EXISTS exam_questions;
DROP TABLE IF EXISTS exam_question_banks;

-- Down migration for 2026-11-22-000079_elearning: drop the
-- E-Learning tables in reverse dependency order.
DROP TABLE IF EXISTS learning_path_courses;
DROP TABLE IF EXISTS learning_paths;
DROP TABLE IF EXISTS reviews;
DROP TABLE IF EXISTS certificates;
DROP TABLE IF EXISTS quiz_attempts;
DROP TABLE IF EXISTS quiz_questions;
DROP TABLE IF EXISTS quizzes;
DROP TABLE IF EXISTS lesson_progress;
DROP TABLE IF EXISTS enrollments;
DROP TABLE IF EXISTS lessons;
DROP TABLE IF EXISTS course_modules;
DROP TABLE IF EXISTS courses;

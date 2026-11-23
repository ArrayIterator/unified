-- Roll back 2026-11-22-000092_curriculum, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS curriculum_settings;
DROP TABLE IF EXISTS curriculum_interventions;
DROP TABLE IF EXISTS curriculum_report_card_entries;
DROP TABLE IF EXISTS curriculum_report_cards;
DROP TABLE IF EXISTS curriculum_lesson_plans;
DROP TABLE IF EXISTS curriculum_grade_descriptors;
DROP TABLE IF EXISTS curriculum_assessment_domains;
DROP TABLE IF EXISTS curriculum_structures;
DROP TABLE IF EXISTS curriculum_basic_competencies;
DROP TABLE IF EXISTS curriculum_core_competencies;
DROP TABLE IF EXISTS curriculum_subjects;
DROP TABLE IF EXISTS curriculum_levels;
DROP TABLE IF EXISTS curriculum_frameworks;

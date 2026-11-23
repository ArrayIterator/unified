-- Roll back migration 2026-11-22-000071_presets: drop the four
-- Presets + Setup Wizard tables. Tables are dropped in reverse
-- dependency order (application and wizard state rows reference
-- companies and presets; module entries reference presets).
-- Dropping the tables also removes the four seeded built-in preset
-- rows. CREATE TABLE count in up.sql (4) matches DROP TABLE count
-- here (4).

DROP TABLE IF EXISTS preset_applications;
DROP TABLE IF EXISTS preset_wizard_states;
DROP TABLE IF EXISTS preset_modules;
DROP TABLE IF EXISTS presets;

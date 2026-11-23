-- Down migration for 2026-11-22-000108_paid_certification_exams: drop
-- the paid-exam activations table and the pricing columns on exams.
DROP TABLE IF EXISTS exam_activations;
ALTER TABLE exams DROP CONSTRAINT IF EXISTS chk_exams_paid_price;
ALTER TABLE exams DROP COLUMN IF EXISTS price_minor;
ALTER TABLE exams DROP COLUMN IF EXISTS is_paid;

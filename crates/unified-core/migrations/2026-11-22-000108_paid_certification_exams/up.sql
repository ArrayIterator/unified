-- Paid certification exams (standalone) for the Online Exams module
-- (`src/modules/exams/`): an exam can be priced and sold on its own,
-- with no course required. A paid exam requires an active paid
-- activation before an attempt can start; activation is created when
-- an order containing the exam is paid (see `on_order_paid`).
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- src/db/mod.rs).
--
-- Notes on the mapping (same as migration 000095_exams: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - `price_minor` is an integer minor amount, following the
--     store/billing `price_minor` convention (BIGINT, never
--     negative). `is_paid` marks the exam as paid; the CHECK
--     constraint keeps the two fields consistent, exactly like the
--     `validate_pricing` helper in the module: a paid exam must
--     carry a price above zero, and a priced exam must be paid.
--   - `exam_activations.user_id` is plain TEXT with NO foreign key
--     on purpose, matching the 000095_exams convention: users live
--     in the users module and this module must not assume its table
--     or migration order. `exam_activations.exam_id` references
--     `exams.id` only by convention, not by a foreign key, for the
--     same reason.
--   - The partial unique index enforces the module's invariant in
--     the database too: at most one active activation per
--     (exam, user). A re-delivered `order.paid` event returns the
--     existing activation instead of creating a duplicate.
--   - All ids are Uuid, all timestamps are Timestamptz.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Paid pricing on exams.
-- ---------------------------------------------------------------------------
ALTER TABLE exams ADD COLUMN IF NOT EXISTS price_minor BIGINT NOT NULL DEFAULT 0 CHECK (price_minor >= 0);
ALTER TABLE exams ADD COLUMN IF NOT EXISTS is_paid BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX IF NOT EXISTS idx_exams_paid ON exams (is_paid) WHERE is_paid;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'chk_exams_paid_price'
    ) THEN
        ALTER TABLE exams ADD CONSTRAINT chk_exams_paid_price
            CHECK ((NOT is_paid OR price_minor > 0) AND (price_minor <= 0 OR is_paid));
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- Paid-exam activations.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS exam_activations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    exam_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    order_id TEXT,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    activated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_exam_activations_exam ON exam_activations (exam_id);
CREATE INDEX IF NOT EXISTS idx_exam_activations_user ON exam_activations (user_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_exam_activations_one_active ON exam_activations (exam_id, user_id) WHERE status = 'active';

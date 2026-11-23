-- Reverse of up.sql for migration 000107_reference_backfill.
-- Drops the backfill columns and their UNIQUE constraints.
-- Note: rows backfilled by up.sql lose their generated references
-- on rollback; re-running up.sql regenerates them (new random part).

ALTER TABLE store_orders DROP CONSTRAINT IF EXISTS store_orders_reference_unique;
ALTER TABLE invoices DROP CONSTRAINT IF EXISTS invoices_reference_unique;
ALTER TABLE store_orders DROP COLUMN IF EXISTS reference;
ALTER TABLE invoices DROP COLUMN IF EXISTS reference;

-- Down migration for 2026-11-22-000096_school_fees: drop the
-- school fee tables in reverse dependency order.
DROP TABLE IF EXISTS school_fee_reminders;
DROP TABLE IF EXISTS school_fee_guardians;
DROP TABLE IF EXISTS school_fee_receipts;
DROP TABLE IF EXISTS school_fee_discounts;
DROP TABLE IF EXISTS school_fee_charges;
DROP TABLE IF EXISTS school_fee_schedules;
DROP TABLE IF EXISTS school_fee_types;

-- Roll back 2026-11-22-000076_pos, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS pos_receipts;
DROP TABLE IF EXISTS pos_refunds;
DROP TABLE IF EXISTS pos_payments;
DROP TABLE IF EXISTS pos_sale_items;
DROP TABLE IF EXISTS pos_sales;
DROP TABLE IF EXISTS pos_shifts;
DROP TABLE IF EXISTS pos_registers;

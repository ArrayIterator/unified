-- Roll back 2026-11-22-000049_procurement, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS procurement_budgets;
DROP TABLE IF EXISTS goods_receipts;
DROP TABLE IF EXISTS purchase_order_approvals;
DROP TABLE IF EXISTS purchase_order_lines;
DROP TABLE IF EXISTS purchase_orders;
DROP TABLE IF EXISTS vendors;

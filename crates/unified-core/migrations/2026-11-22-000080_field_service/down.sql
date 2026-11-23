-- Roll back migration 2026-11-22-000080_field_service.
-- Drops the 7 field service tables in reverse dependency order
-- (sign-offs and dispatch assignments first, equipment last).

DROP TABLE IF EXISTS field_signoffs;
DROP TABLE IF EXISTS field_dispatch_assignments;
DROP TABLE IF EXISTS field_work_order_parts;
DROP TABLE IF EXISTS field_work_order_items;
DROP TABLE IF EXISTS field_work_orders;
DROP TABLE IF EXISTS field_maintenance_plans;
DROP TABLE IF EXISTS field_equipment;

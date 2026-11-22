-- Roll back 2026-11-22-000062_logistics, in reverse dependency order.
-- The core tables from 2026-11-22-000001_core are untouched.
-- Note the creation order inside up.sql: shipments references
-- drivers / vehicles / warehouses, so those tables must be dropped
-- after the tables that reference them (shipments, stock movements,
-- route stops, proofs, exceptions, routes).

DROP TABLE IF EXISTS logistics_rate_tables CASCADE;
DROP TABLE IF EXISTS logistics_exceptions CASCADE;
DROP TABLE IF EXISTS logistics_proofs CASCADE;
DROP TABLE IF EXISTS logistics_route_stops CASCADE;
DROP TABLE IF EXISTS logistics_routes CASCADE;
DROP TABLE IF EXISTS logistics_stock_movements CASCADE;
DROP TABLE IF EXISTS logistics_shipment_events CASCADE;
DROP TABLE IF EXISTS logistics_shipments CASCADE;
DROP TABLE IF EXISTS logistics_warehouses CASCADE;
DROP TABLE IF EXISTS logistics_vehicles CASCADE;
DROP TABLE IF EXISTS logistics_drivers CASCADE;

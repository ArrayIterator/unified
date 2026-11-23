-- Roll back 2026-11-22-000025_gateway, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS gateway_request_log;
DROP TABLE IF EXISTS gateway_routing_rules;

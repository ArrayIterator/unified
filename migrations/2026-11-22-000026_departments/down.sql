-- Roll back 2026-11-22-000026_departments, in reverse dependency
-- order. The core tables from 2026-11-22-000001_core are untouched.

DROP TABLE IF EXISTS department_routing_rules;
DROP TABLE IF EXISTS departments;

-- Roll back 2026-11-22-000017_load_balancer, in reverse
-- dependency order. The core tables from 2026-11-22-000001_core
-- are untouched.

DROP TABLE IF EXISTS lb_backend_stats;
DROP TABLE IF EXISTS lb_backends;
DROP TABLE IF EXISTS load_balancers;

-- Reverse of `up.sql` for migration 000100_tenant_isolation.
-- Drops the control-plane tenant tables in dependency order
-- (grants and provisioning steps first, tenants last). Every
-- CREATE TABLE in `up.sql` has exactly one DROP TABLE here.
--
-- WARNING: this drops the tenant registry itself. It never drops a
-- tenant schema: per-tenant schemas are owned by the destroy flow
-- (builder B3, `tenant_destroy.rs`), never by this migration.

DROP TABLE IF EXISTS tenant_module_grants;
DROP TABLE IF EXISTS tenant_provisioning_steps;
DROP TABLE IF EXISTS tenants;

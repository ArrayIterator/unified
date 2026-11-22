-- Down migration for 2026-11-22-000098_entitlements: drop the
-- per-product entitlement tables in reverse dependency order.
-- The downgrade/expiry flow never deletes tenant data; this down
-- migration is an administrator-driven schema rollback only, and
-- it drops the entitlement bookkeeping tables themselves, never
-- any tenant data table.
DROP TABLE IF EXISTS entitlement_module_activations;
DROP TABLE IF EXISTS entitlement_settings;
DROP TABLE IF EXISTS entitlement_package_changes;
DROP TABLE IF EXISTS entitlement_denied_attempts;
DROP TABLE IF EXISTS entitlement_history;
DROP TABLE IF EXISTS entitlements;

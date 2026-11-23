-- Reverse of 000127: drop the C12 provider registry table.
-- The runtime registries fall back to the embedded seed document
-- (crates/unified-core/provider_registry_seed.json), so dropping the
-- table only removes the durable record — including any
-- admin-registered providers, which are NOT recoverable from the
-- seed. Export custom rows before rolling back.
-- (Indexes on the table are dropped with it.)

DROP TABLE IF EXISTS provider_registry_definitions;

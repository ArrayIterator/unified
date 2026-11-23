-- C11 DB read path (task 271): tenant scoping columns for the
-- tables this slice wires to the database.
--
-- No new tables: every table below already exists (000123 for
-- `finalize_jobs`, 000014 for the payment tables). What was
-- missing is the tenant stamp the C3 guards assume:
--   - `finalize_jobs` had no tenant column at all, so the
--     finalize write-through in `src/modules/documents/state/`
--     `finalize.rs` could not persist the job's tenant scope.
--     The column is a plain nullable UUID with NO foreign key:
--     tenant ids resolve through the tenancy middleware, and
--     the tenants table may not be visible in a tenant
--     schema's search path (same convention as
--     `documents.company_id` and the ESG/forum reference ids,
--     which also carry no foreign key).
--   - The payment money tables (`payment_intents`,
--     `payment_transactions`, `payment_methods`,
--     `gateway_subscriptions`) had no tenant column either.
--     The runtime payment records carry no `tenant_id` field
--     yet, so these columns are stamped by the future
--     payments write-through, not by this slice; adding them
--     now keeps the DDL ahead of the code instead of behind
--     it. NULL means "written before C11 stamping".
-- ESG (`000119`) and forums (`000110`) already carry
-- `tenant_id TEXT NOT NULL` on every row: nothing to add
-- for them here (their Diesel schema/models are added in
-- this slice instead — they never had any).
-- All statements are idempotent (`IF NOT EXISTS`), so a
-- re-run after a partial apply is safe.

-- 1. `finalize_jobs.tenant_id`: the tenant scope the object
-- is stored under (copied from the upload session at job
-- creation). NULL only for jobs created before this
-- migration; a job with no tenant fails closed at verify
-- time, exactly like the in-memory worker.
ALTER TABLE IF EXISTS finalize_jobs
    ADD COLUMN IF NOT EXISTS tenant_id UUID;

CREATE INDEX IF NOT EXISTS idx_finalize_jobs_tenant_id
    ON finalize_jobs (tenant_id);

-- 2. Payment money tables: tenant stamp (TEXT, same
-- convention as the ESG/forum `tenant_id` columns).
ALTER TABLE IF EXISTS payment_intents
    ADD COLUMN IF NOT EXISTS tenant_id TEXT;

CREATE INDEX IF NOT EXISTS idx_payment_intents_tenant_id
    ON payment_intents (tenant_id);

ALTER TABLE IF EXISTS payment_transactions
    ADD COLUMN IF NOT EXISTS tenant_id TEXT;

CREATE INDEX IF NOT EXISTS idx_payment_transactions_tenant_id
    ON payment_transactions (tenant_id);

ALTER TABLE IF EXISTS payment_methods
    ADD COLUMN IF NOT EXISTS tenant_id TEXT;

CREATE INDEX IF NOT EXISTS idx_payment_methods_tenant_id
    ON payment_methods (tenant_id);

ALTER TABLE IF EXISTS gateway_subscriptions
    ADD COLUMN IF NOT EXISTS tenant_id TEXT;

CREATE INDEX IF NOT EXISTS idx_gateway_subscriptions_tenant_id
    ON gateway_subscriptions (tenant_id);

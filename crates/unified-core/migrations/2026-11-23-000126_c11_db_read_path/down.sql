-- Reverse of 000126: drop the C11 tenant columns and their
-- indexes. Dropping a column discards the stamped tenant
-- scope; jobs and payment rows written while the columns
-- existed keep every other field.

DROP INDEX IF EXISTS idx_gateway_subscriptions_tenant_id;
ALTER TABLE IF EXISTS gateway_subscriptions
    DROP COLUMN IF EXISTS tenant_id;

DROP INDEX IF EXISTS idx_payment_methods_tenant_id;
ALTER TABLE IF EXISTS payment_methods
    DROP COLUMN IF EXISTS tenant_id;

DROP INDEX IF EXISTS idx_payment_transactions_tenant_id;
ALTER TABLE IF EXISTS payment_transactions
    DROP COLUMN IF EXISTS tenant_id;

DROP INDEX IF EXISTS idx_payment_intents_tenant_id;
ALTER TABLE IF EXISTS payment_intents
    DROP COLUMN IF EXISTS tenant_id;

DROP INDEX IF EXISTS idx_finalize_jobs_tenant_id;
ALTER TABLE IF EXISTS finalize_jobs
    DROP COLUMN IF EXISTS tenant_id;

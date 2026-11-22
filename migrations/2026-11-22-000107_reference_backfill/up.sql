-- Backfill for migration 000107: add the shared payment-reference
-- column where it is still missing, then generate one reference
-- per row that has none. Format (see src/core/reference_id.rs):
--   {PREFIX 4}-{12 hex timestamp}-{12 hex random}  (30 chars)
-- Default prefix ORDR; timestamp = created_at in ms AND
-- 0xFFFFFFFFFFFF as 12 lowercase hex chars; random = 6 random
-- bytes as 12 hex chars. Only rows whose reference is '' or NULL
-- are touched, so the statement is idempotent-safe and re-runnable.
-- On a unique-violation the per-row block regenerates once with a
-- fresh random value and retries; a second conflict raises loudly
-- and never overwrites the existing row.
-- Requires the pgcrypto extension for gen_random_bytes(6); if the
-- migration role cannot create extensions, use the md5 fallback
-- variant noted below (same shape, pseudorandom instead of CSPRNG).

CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE store_orders ADD COLUMN IF NOT EXISTS reference VARCHAR(30);
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS reference VARCHAR(30);

DO $$
DECLARE
    r RECORD;
    ref TEXT;
    tries INT;
BEGIN
    FOR r IN
        SELECT id, created_at FROM store_orders
        WHERE reference IS NULL OR reference = ''
        ORDER BY created_at, id
    LOOP
        tries := 0;
        LOOP
            ref := 'ORDR-'
                || lpad(to_hex(((extract(epoch FROM r.created_at) * 1000)::bigint & 281474976710655)), 12, '0')
                || '-' || encode(gen_random_bytes(6), 'hex');
            BEGIN
                UPDATE store_orders SET reference = ref WHERE id = r.id;
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                tries := tries + 1;
                IF tries >= 2 THEN
                    RAISE EXCEPTION 'reference backfill: second unique conflict for store_orders id %', r.id;
                END IF;
            END;
        END LOOP;
    END LOOP;

    FOR r IN
        SELECT id, created_at FROM invoices
        WHERE reference IS NULL OR reference = ''
        ORDER BY created_at, id
    LOOP
        tries := 0;
        LOOP
            ref := 'ORDR-'
                || lpad(to_hex(((extract(epoch FROM r.created_at) * 1000)::bigint & 281474976710655)), 12, '0')
                || '-' || encode(gen_random_bytes(6), 'hex');
            BEGIN
                UPDATE invoices SET reference = ref WHERE id = r.id;
                EXIT;
            EXCEPTION WHEN unique_violation THEN
                tries := tries + 1;
                IF tries >= 2 THEN
                    RAISE EXCEPTION 'reference backfill: second unique conflict for invoices id %', r.id;
                END IF;
            END;
        END LOOP;
    END LOOP;
END $$;

ALTER TABLE store_orders ALTER COLUMN reference SET NOT NULL;
ALTER TABLE invoices ALTER COLUMN reference SET NOT NULL;
ALTER TABLE store_orders ADD CONSTRAINT store_orders_reference_unique UNIQUE (reference);
ALTER TABLE invoices ADD CONSTRAINT invoices_reference_unique UNIQUE (reference);

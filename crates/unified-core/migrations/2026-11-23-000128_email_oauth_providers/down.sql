-- Reverse of 000128: remove the 4 email providers
-- and the logo column. Only the 4 built-in ids are deleted; rows an
-- admin registered themselves are untouched. The runtime registry
-- falls back to the embedded seed document, so the providers keep
-- working in memory until the seed is reverted too.
DELETE FROM provider_registry_definitions
    WHERE family = 'email'
      AND provider_id IN ('gmail', 'yahoo', 'outlook', 'sendmail');
ALTER TABLE provider_registry_definitions DROP COLUMN IF EXISTS logo;
